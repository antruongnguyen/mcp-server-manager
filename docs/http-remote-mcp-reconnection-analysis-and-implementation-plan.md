# MCPSM Remote HTTP Reconnection Analysis & Implementation Plan

> **Status:** Plan reviewed against source (2026-09-11). The recommended minimal fix is
> to reconnect remote Streamable HTTP MCP servers after their first failed active health
> probe while retaining the existing two-failure tolerance for STDIO servers. Two
> correctness details are included in the plan: stale probe results must not evict a newly
> connected client, and an evicted HTTP client should be explicitly cancelled. Awaiting
> owner approval before implementation.

## 1. Executive Summary

When a laptop sleeps overnight, MCPSM and its Tokio runtime are suspended and cannot send periodic health probes. Local STDIO MCP child processes are suspended with the laptop and normally resume with their pipes and process state intact. A remote Streamable HTTP MCP server, however, can expire the MCP session, close an idle connection, or discard related server-side state while the laptop is asleep.

After wake, MCPSM still has an `Arc<McpClient>` representing the pre-sleep HTTP session. Depending on how the remote endpoint and HTTP transport report expiration:

* `McpClient::is_closed()` may immediately report a closed transport, in which case the existing passive health check already reconnects it; or
* the transport may still appear open while the next MCP request errors or times out. This is detected by the active `list_all_tools()` probe, but the current code waits for **two consecutive failed probes** before eviction and restart.

The recommended change is deliberately small:

1. Remote HTTP servers use a failed-probe threshold of **1**.
2. STDIO servers retain the existing threshold of **2** to tolerate a transiently busy local process.
3. A failed probe result is applied only if it belongs to the client instance that is still current, preventing an old in-flight probe from evicting a newly reconnected client.
4. Eviction explicitly cancels the removed MCP client before starting a replacement connection.
5. The existing `evict_and_maybe_restart` and `handle_start_server` pipeline remains the only recovery path.

No macOS wake listener, dedicated keepalive task, new dependency, dashboard setting, or configuration migration is required.

---

## 2. Current Behavior and Root Cause

### 2.1 Transport behavior during laptop sleep

MCPSM supports two relevant child transports through `ServerConfig` (`crates/mcpsm-core/src/core/server.rs`):

* **STDIO:** a local command is spawned and MCP messages use the process's stdin/stdout pipes.
* **Remote HTTP:** MCPSM creates an rmcp `StreamableHttpClientTransport` for the configured URL in `crates/mcpsm-core/src/mcp/client.rs::connect_http`.

Laptop sleep affects them differently:

* A STDIO process and MCPSM are suspended together. Neither side observes hours of active runtime, and the local pipes normally remain valid after wake.
* A remote HTTP server continues running independently. Its idle/session timeout continues advancing and may invalidate MCPSM's session before the laptop wakes.

A keepalive cannot prevent this while the machine is asleep because MCPSM executes no code during sleep. Recovery must therefore create a fresh HTTP transport and repeat the MCP initialization handshake after wake.

### 2.2 Existing health-check mechanism

`ServerManager` already contains almost all required recovery behavior:

* `health_check_interval` ticks every 30 seconds and uses `MissedTickBehavior::Delay`.
* `run_health_checks()` snapshots all `Ready` MCP clients without holding the shared lock across child awaits.
* Each client is actively probed with `client.list_all_tools()` under `HEALTH_PROBE_TIMEOUT` (10 seconds).
* Probe work runs in detached tasks and returns through `probe_result_tx`, so a slow endpoint does not block the manager event loop.
* `handle_probe_result()` counts consecutive failures and currently evicts every transport after `MAX_PROBE_FAILURES` (2).
* A second, passive pass checks `McpClient::is_closed()` and immediately evicts clients whose transport is known to be closed.
* Both failure paths call `evict_and_maybe_restart()`, which removes stale state and starts the configured server again.

The recovery infrastructure therefore already exists. The behavior gap is the uniform two-failure threshold, which is unnecessarily conservative for an HTTP session known to be replaceable.

### 2.3 Why the passive closed check is insufficient

An expired remote MCP session does not have to make rmcp's `RunningService::is_closed()` return `true` immediately. For example, the underlying client may discover the expiration only when the next request receives an HTTP error or waits until timeout.

In that case:

1. `is_closed()` remains false.
2. The active `tools/list` probe fails.
3. `handle_probe_result()` records only the first failure.
4. MCPSM waits for another health-check cycle and another failed request before reconnecting.

This explains why HTTP servers can remain unavailable after wake even while STDIO servers continue working.

### 2.4 Expected recovery timing

With a one-failure threshold, an expired HTTP session is recycled on the first failed active probe:

* If an overdue timer tick is delivered promptly after wake and the request fails immediately, reconnection begins almost immediately.
* If the request hangs, detection takes up to `HEALTH_PROBE_TIMEOUT` (10 seconds), followed by connection and MCP initialization time.
* If no overdue tick is delivered immediately, detection waits for the next 30-second health-check tick, then up to the probe timeout.

These are expected bounds, not a hard wake-time guarantee; OS resume scheduling and network restoration can add delay.

---

## 3. Design Review and Corrections

### 3.1 Recommended policy

Use a transport-aware failed-probe threshold:

| Effective transport | Failed probes before eviction | Reason |
|---|---:|---|
| Remote HTTP (`url`, no selected `command`) | 1 | A failed post-sleep request commonly indicates an expired disposable session; reconnecting is cheap and appropriate. |
| STDIO (`command`) | 2 | A local process may be briefly busy; retaining one retry avoids unnecessary process termination. |

The transport classification must match `start_servers_batch()` (the function that actually selects the connection path; `handle_start_server()` delegates to it), which gives `command`/STDIO precedence via `if config.is_stdio() { … } else if config.is_remote() { … }`. A configuration containing both `command` and `url` must therefore use the STDIO threshold unless configuration validation is changed separately.

Recommended helper:

```rust
fn max_probe_failures(config: &ServerConfig) -> u8 {
    if config.is_remote() && !config.is_stdio() {
        1
    } else {
        MAX_PROBE_FAILURES
    }
}
```

Keep `MAX_PROBE_FAILURES = 2` as the default/STDIO policy. Do not add a user-facing setting until there is evidence that threshold tuning is needed.

### 3.2 Reuse the existing restart pipeline

Do not implement a separate HTTP retry loop. After the threshold is reached, continue to call:

```rust
self.evict_and_maybe_restart(id, reason).await;
```

That path already:

* removes the client from proxy-visible shared state;
* clears tools, resources, resource templates, prompts, and peer metadata;
* publishes `Error` and subsequent startup status changes;
* honors the disabled flag;
* applies the existing restart-budget guard; and
* calls `handle_start_server()`, which delegates to `start_servers_batch()`; that function selects `spawn_remote_connect()` and performs a fresh Streamable HTTP connection and MCP handshake.

This is the smallest change and keeps manual, passive-health, and active-health recovery behavior aligned.

### 3.3 Required correction: reject stale probe results

The current probe channel carries only `(server_id, succeeded)`. That is insufficient to prove that the result belongs to the client currently registered under that ID.

A race is possible:

1. A probe starts against old HTTP client **A**.
2. Passive `is_closed()` detection or a manual action evicts **A** and starts client **B**.
3. **B** reaches `Ready`.
4. The delayed failed result for **A** reaches `handle_probe_result()`.
5. Because the server ID is `Ready` again, the old result can increment the new server's counter and—under the proposed one-failure HTTP threshold—incorrectly evict **B**.

The lower threshold makes this existing race more consequential. Note the race is **narrow and rare**: with `MissedTickBehavior::Delay` and a 10s probe timeout inside a 30s tick, a tick's probe tasks normally complete and drain before the next tick fires, so the realistic trigger is a manual stop/start or Pass 2 (`is_closed()`) eviction racing an in-flight Pass 1 probe. It is real enough to guard against but does not warrant a heavy async test harness (see §5.2-D). The implementation must associate each result with the probed client instance and ignore it unless that same instance is still current.

Minimal approach:

```rust
struct ProbeResult {
    id: String,
    client: Arc<McpClient>,
    ok: bool,
}
```

Before changing counters, compare the result's client with the current entry using `Arc::ptr_eq`. If there is no current entry or it points to another client, discard the result.

A numeric connection generation would also work, but it adds state and update sites with no benefit for this case.

### 3.4 Required cleanup: cancel the evicted client

`handle_stop_server()` explicitly cancels a removed MCP client, but `evict_and_maybe_restart()` currently removes the entry without cancellation. For remote HTTP clients there is no child process to kill, so explicit cancellation is the direct way to stop the stale rmcp service and release its transport resources.

Change eviction from a bare removal to:

```rust
if let Some(client) = self.shared_mcp_clients.write().await.remove(id) {
    client.cancellation_token().cancel();
}
```

This is safe even if an active probe still holds another `Arc`; cancellation tells that running service to shut down, while the `Arc` controls only object lifetime.

### 3.5 Restart-budget limitation

`evict_and_maybe_restart()` allows at most three health-triggered restarts within a five-minute window. However, it schedules one connection attempt per eviction. If the laptop's network is not usable yet and that reconnect attempt itself fails, `handle_connect_result()` leaves the server in `Error`; it does not automatically schedule another attempt.

This plan does **not** add connection-attempt retries. The intended fix is faster recycling of an expired established HTTP session. If field testing shows that macOS commonly reports network availability after the first post-wake reconnect attempt, a separate bounded retry/backoff change should follow rather than being folded into this threshold adjustment.

---

## 4. Implementation Plan

Each step includes a verification condition.

### Step 1: Add a transport-aware threshold helper

**File:** `crates/mcpsm-core/src/core/manager.rs`

Add a private pure helper near the existing health constants:

```rust
/// Remote HTTP sessions are disposable and should reconnect on their first
/// failed probe; local STDIO processes retain one transient-failure retry.
fn max_probe_failures(config: &ServerConfig) -> u8 {
    if config.is_remote() && !config.is_stdio() {
        1
    } else {
        MAX_PROBE_FAILURES
    }
}
```

Keep:

```rust
const MAX_PROBE_FAILURES: u8 = 2;
```

This avoids renaming unrelated code while making its role as the default threshold clear through the helper and comment.

**Verify:** HTTP-only configuration returns `1`; command-based and malformed/non-running configurations return the default `2`; a command-plus-URL configuration matches the actual STDIO-first startup behavior in `start_servers_batch()` (`is_stdio()` checked before `is_remote()`) and returns `2`.

### Step 2: Attach client identity to probe results

Replace the probe channel tuple with a small private result struct:

```rust
struct ProbeResult {
    id: String,
    client: Arc<McpClient>,
    ok: bool,
}
```

Update these fields and call sites:

1. `ServerManager::probe_result_tx` — from `Sender<(String, bool)>` to `Sender<ProbeResult>`
2. `ServerManager::probe_result_rx` — from `Receiver<(String, bool)>` to `Receiver<ProbeResult>`
3. Channel construction in `ServerManager::new()`
4. The probe-result branch in `ServerManager::run()` — currently `if let Some((id, ok)) = probe { self.handle_probe_result(&id, ok).await; }`; change to bind the struct and pass it through
5. The detached probe task in `run_health_checks()` — send a `ProbeResult` instead of `(id, ok)`
6. `handle_probe_result()` signature — change from `handle_probe_result(&mut self, id: &str, ok: bool)` to `handle_probe_result(&mut self, result: ProbeResult)`, and update all internal uses of `id`/`ok` to `result.id`/`result.ok`

Each spawned probe should retain the same `Arc<McpClient>` that it queried and send it back with the result:

```rust
let ok = matches!(
    tokio::time::timeout(HEALTH_PROBE_TIMEOUT, client.list_all_tools()).await,
    Ok(Ok(_))
);
ProbeResult { id, client, ok }
```

**Verify:** the manager compiles with no remaining `(String, bool)` probe-result signatures.

### Step 3: Ignore results from replaced clients

At the start of `handle_probe_result()`, confirm that the result belongs to the currently registered client:

```rust
let is_current_client = {
    let clients = self.shared_mcp_clients.read().await;
    clients
        .get(&result.id)
        .is_some_and(|current| Arc::ptr_eq(current, &result.client))
};

if !is_current_client {
    return;
}
```

Then retain the existing `Ready` status check. Both checks are useful:

* pointer identity rejects results from an old connection after reconnection;
* status rejects results while the same client is being stopped or transitioned.

Do not hold the shared-client read guard while mutating manager state or awaiting eviction.

**Verify:** code inspection confirms the read guard is scoped and dropped before `evict_and_maybe_restart().await`.

### Step 4: Apply the transport-aware threshold

In `handle_probe_result()`, replace the uniform threshold comparison:

```rust
server.probe_failures >= MAX_PROBE_FAILURES
```

with:

```rust
let threshold = max_probe_failures(&server.config);
server.probe_failures = server.probe_failures.saturating_add(1);
server.probe_failures >= threshold
```

Preserve the success behavior:

```rust
server.probe_failures = 0;
```

Capture whether the server uses remote HTTP while the mutable server borrow is active so the subsequent logging can identify the recovery type without another ambiguous transport check.

Expected behavior:

* first HTTP probe failure: evict and reconnect;
* first STDIO probe failure: increment only;
* second consecutive STDIO failure: evict and restart;
* any successful probe: reset the consecutive-failure counter.

**Verify:** no change is made to `HEALTH_PROBE_TIMEOUT`, the 30-second interval, or passive `is_closed()` handling.

### Step 5: Make recovery logs transport-specific

When the first HTTP probe failure reaches its threshold, log:

```text
[mcpsm] HTTP health probe failed; reconnecting...
```

Pass a specific reason into the existing recovery helper:

```text
Remote HTTP connection unresponsive (detected by health probe)
```

Retain the current generic STDIO/unresponsive wording for other transports:

```text
[mcpsm] Health check failed: server unresponsive
```

This distinction makes the expected post-wake recovery visible without adding dashboard UI.

**Verify:** dashboard logs clearly show that MCPSM selected HTTP reconnection rather than restarting a local process.

### Step 6: Cancel a client when evicting it

In `evict_and_maybe_restart()`, capture the removed client and cancel its rmcp service. The current line is a bare `self.shared_mcp_clients.write().await.remove(id);` whose result is discarded; replace it with:

```rust
if let Some(client) = self.shared_mcp_clients.write().await.remove(id) {
    client.cancellation_token().cancel();
}
```

**Critical:** the write guard on `shared_mcp_clients` must be released at the end of this `if let` statement (the temporary guard is dropped when the `remove()` expression's enclosing statement ends). The very next block awaits `process::stop_server(child).await`; holding the client write lock across that child await would violate the lock discipline documented in CLAUDE.md. Do not widen the guard's scope to enclose the child shutdown.

This affects both HTTP and STDIO health eviction consistently. STDIO eviction will still stop the child process through `process::stop_server()`.

**Verify:** the shared-client write guard is released at the end of the `if let` statement before process shutdown or any other await.

### Step 7: Add focused unit tests

Add a `#[cfg(test)]` module in `manager.rs` for the pure threshold policy. Construct minimal `ServerConfig` values and test:

1. URL-only remote configuration uses threshold `1`.
2. Command-only STDIO configuration uses threshold `2`.
3. Command-plus-URL configuration uses threshold `2`, matching the STDIO-first connection selection.

If the failure-count transition is extracted into a pure helper during implementation, test success reset and threshold crossing there as well. Do not introduce an abstraction solely to test two lines of mutation.

The stale-client race is best covered by an async manager test only if the existing test setup can construct usable `McpClient` instances cheaply. Otherwise, verify it through the manual replacement scenario in §5.2 rather than adding a large mock framework.

**Verify:** the threshold tests fail if HTTP is accidentally returned to the global two-failure behavior.

### Step 8: Update release notes

**File:** `CHANGELOG.md`

Under `[Unreleased]`, add:

```markdown
- Remote HTTP MCP servers now reconnect after the first failed health probe, allowing expired sessions to recover promptly after laptop sleep. STDIO servers retain the existing two-failure tolerance.
```

Optionally mention stale-probe protection in the same entry if it is useful to maintainers, but avoid a separate user-facing bullet for an internal race guard.

**Verify:** the wording promises prompt recovery, not an OS-level immediate wake notification or unlimited retry behavior.

---

## 5. Verification and Safety Measures

### 5.1 Automated verification

Run in order:

```bash
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace
cargo test -p mcpsm-core
cargo test --workspace
cargo build --workspace
```

If the macOS application is the release target, also run:

```bash
./scripts/build-app.sh
```

### 5.2 Functional verification

#### A. Primary sleep/wake case

1. Start one remote HTTP MCP server and one local STDIO MCP server.
2. Confirm both reach `Ready` and expose tools through the unified proxy.
3. Put the laptop to sleep long enough for the remote server's idle/session timeout to expire.
4. Wake the laptop and wait for network connectivity to return.
5. Confirm the first failed HTTP health probe logs the reconnect action.
6. Confirm the HTTP server performs a new MCP handshake and returns to `Ready`.
7. Confirm the STDIO server retains its original process/PID and is not restarted.
8. Confirm tools from both servers are again available through the proxy.

#### B. HTTP first-failure policy

Use a test HTTP MCP endpoint with a short session TTL or make the endpoint reject the existing session after initialization. Confirm one failed probe is sufficient to transition through `Error`/`Starting`/`Initializing` back to `Ready`; do not wait for a second health tick.

#### C. STDIO tolerance regression

Temporarily make a STDIO MCP child fail or time out for exactly one probe, then recover. Confirm:

* the first failure does not restart the process;
* a successful next probe resets the counter; and
* two consecutive failures still use the existing eviction/restart path.

#### D. Stale-result race

Force a probe against HTTP client A to remain pending, manually restart the server so client B becomes current, then allow A's probe to fail. Confirm the stale result is ignored and B remains `Ready`.

#### E. Network-not-ready limitation

Wake the laptop while networking is deliberately unavailable. Confirm the reconnect attempt can fail into `Error`, and document whether users commonly hit this timing. If this is reproducible in normal operation, open a follow-up for bounded reconnect retries with backoff; do not hide that behavior by increasing the health-probe threshold again.

### 5.3 Observability checks

During manual tests, confirm logs make these states distinguishable:

* health probe failed for remote HTTP;
* stale client removed and reconnect initiated;
* MCP handshake completed after reconnect;
* reconnect failed because the endpoint/network remained unavailable;
* restart budget exhausted, if repeated established sessions fail within the existing window.

No new metrics or persistent counters are required for this change.

---

## 6. Risks and Trade-offs

### 6.1 False-positive HTTP reconnect

A single slow HTTP response can trigger reconnection even when the server session is valid. The probe already allows 10 seconds, so this policy treats a request exceeding that bound as unhealthy. This is an intentional trade-off for faster post-sleep recovery.

If legitimate remote MCP operations routinely block `tools/list` beyond 10 seconds, address the server or revisit the probe endpoint/timeout; do not restore a global threshold that delays all expired-session recovery.

### 6.2 Probe request cost

The existing health check already sends `tools/list` every 30 seconds. This plan does not add traffic or increase probe frequency. It changes only the response to the first failure.

### 6.3 Restart storms

The existing `MAX_RESTART_ATTEMPTS` and `RESTART_WINDOW` logic limits repeated health-triggered restarts. This plan continues to use that guard.

As noted in §3.5, a failed connection attempt is not itself repeatedly retried by the current manager. A separate retry policy should use bounded exponential backoff if later required.

### 6.4 Timer behavior after sleep

This solution reacts to the next health tick; it does not subscribe to macOS power notifications. Recovery is therefore prompt but not guaranteed to begin at the exact wake event.

Adding an OS wake listener would increase platform-specific code and still need the same safe reconnection path. Defer it unless measured recovery latency remains unacceptable after this fix.

---

## 7. Scope Boundary and Acceptance Criteria

### 7.1 In scope

* One-failure eviction policy for effective remote HTTP transports.
* Existing two-failure policy retained for STDIO.
* Reuse of existing eviction and restart machinery.
* Protection against stale probe results affecting replacement clients.
* Explicit cancellation of evicted clients.
* Focused policy tests, release note, and manual sleep/wake validation.

### 7.2 Out of scope

* Preventing remote session expiration while the laptop is asleep.
* macOS sleep/wake event integration.
* Configurable health intervals or thresholds.
* A new MCP ping implementation.
* Persistent retry loops for connection attempts that fail while networking is unavailable.
* Changes to remote server timeout configuration.

### 7.3 Acceptance criteria

The fix is complete when:

1. A URL-based MCP server is evicted after its first failed active health probe.
2. A command-based MCP server still requires two consecutive failed probes.
3. A successful probe resets the failure counter.
4. A result from an old client instance cannot evict or modify the state of its replacement.
5. The removed client is cancelled before replacement.
6. The existing restart budget and disabled-server behavior remain intact.
7. After an overnight sleep with an expired HTTP session, the remote server reconnects on the first post-wake failed probe while the STDIO server remains running.
8. Workspace formatting, checks, tests, lint, and build complete successfully.
