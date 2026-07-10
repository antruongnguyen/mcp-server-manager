# MCPSM Part C — Evicting & Restarting Unresponsive Clients

> **Status:** Draft plan (2026-07-10). Follow-up to Parts A + B in
> `stability-analysis-and-implementation-plan.md`. **Implement this only after
> Parts A + B are merged and verified** — it builds directly on the per-child
> timeout constant introduced in Part B (Step 6). Awaiting owner review/decision.

## 1. Scope & Motivation

Parts A + B stop a hung child MCP server from freezing the manager loop or the dashboard (A) and from hanging the aggregate `/mcp` response (B). After those land, a hung-but-open child degrades gracefully: it is **skipped** on each proxy request after a timeout window.

The remaining inefficiency: **a permanently unresponsive child is still queried on every proxy request**, costing one full `CHILD_REQUEST_TIMEOUT` window (30s) each time until the user manually restarts it. Part C makes MCPSM detect that condition automatically and route the child through the recovery machinery that already exists.

### 1.1 What already exists (verified in `crates/mcpsm-core/src/core/manager.rs`)
`run_health_checks` (manager.rs:1197-1261) already runs every 30s (`health_check_interval`, manager.rs:211-213) and:
1.  Finds `Ready` servers whose MCP client reports **`c.is_closed()`** (manager.rs:1200-1211).
2.  Removes the dead client from `shared_mcp_clients`, kills the child process, clears tools/resources/prompts/peer_info, and sets status to `Error` (manager.rs:1220-1245).
3.  **Auto-restarts** the server if not disabled, via `handle_start_server` (manager.rs:1247-1259).

So the eviction + restart pipeline is **already built**. Part C does not reinvent it.

### 1.2 The actual gap
`RunningService::is_closed()` only returns true when the transport/connection has **actually closed** (child exited, pipe broke, HTTP connection dropped). It does **not** detect a child that is:
*   still connected (transport open, process alive), but
*   **hung** — no longer answering requests (deadlocked, wedged on a blocking syscall, GC-paused for minutes, stuck on a lost upstream dependency).

This "alive-but-hung" child is exactly the multi-day failure mode. It never trips `is_closed()`, so `run_health_checks` never evicts it, so Part B's timeout fires on every single proxy request indefinitely.

**Part C's job:** teach the health check to detect alive-but-hung children (via an active liveness probe) and feed them into the existing eviction/restart path.

---

## 2. Design

### 2.1 Strategy: active liveness probe in the health check
Extend `run_health_checks` so that, in addition to the passive `is_closed()` check, it **actively probes** each `Ready` client with a cheap, bounded request. A client that fails the probe (times out or errors) N consecutive times is treated the same as a closed connection: evicted and auto-restarted through the existing code path.

**Probe choice:** the cheapest standard MCP round-trip. Options, in preference order:
1.  `client.list_all_tools()` wrapped in `tokio::time::timeout` — already available, no new capability needed, exercises the full request/response path. **Recommended.**
2.  An MCP `ping` if/when the rmcp client wrapper exposes one — lighter, but confirm availability before choosing.

Use the same `CHILD_REQUEST_TIMEOUT` constant from Part B (or a dedicated, shorter `HEALTH_PROBE_TIMEOUT`, e.g. 10s — decide in review).

### 2.2 Consecutive-failure threshold (avoid false positives)
A single slow probe must **not** evict a healthy-but-momentarily-busy server. Require **N consecutive failed probes** (default `N = 2`, i.e. ~60s of unresponsiveness at the 30s tick) before eviction. A single success resets the counter.

This needs a small piece of per-server state: a failure counter on `ManagedServer`.

### 2.3 Probes must not re-introduce the Part A stall
The health check runs **inside the manager's `select!` loop** (manager.rs:288-290) and awaits inline. If we probe children sequentially while holding any shared lock, or if a probe blocks without a timeout, we recreate the freeze Part A fixed. Rules:
*   **Clone the `Arc<McpClient>` handles into a local `Vec` first, dropping the `shared_mcp_clients` read guard before probing** (same pattern as `handle_tool_refresh`, manager.rs:1275+).
*   **Every probe is wrapped in `tokio::time::timeout`** — no unbounded await.
*   Probes for multiple servers run **concurrently** (`futures::future::join_all` or `JoinSet`) so one slow probe doesn't serialize the others and blow the health-check budget.

### 2.4 Interaction with auto-restart loops
The existing auto-restart (manager.rs:1247-1259) already fires on `is_closed()`. Adding probe-based eviction means a genuinely broken server (crashes on startup, then hangs, repeat) could enter a restart loop every ~60s. Decide in review whether to:
*   (a) Keep it simple — rely on the fact that a server that won't start lands in `Error` and isn't probed (only `Ready` servers are probed). A server that starts, goes `Ready`, then hangs will restart each cycle. Acceptable for v1.
*   (b) Add restart backoff / a max-consecutive-restart cap that leaves the server in `Error` for the user to fix. More robust; more code. **Recommended as a fast-follow, not a blocker.**

---

## 3. Implementation Plan

Each step has a verification check. Steps build on the Part B timeout constant.

### Step 1: Add a probe-failure counter to `ManagedServer`
In `crates/mcpsm-core/src/core/manager.rs`, add a field to the `ManagedServer` struct (manager.rs:75-86):

```rust
struct ManagedServer {
    // ... existing fields ...
    child: Option<tokio::process::Child>,
    /// Consecutive failed liveness probes; reset to 0 on any successful probe.
    probe_failures: u8,
}
```

Initialize `probe_failures: 0` at every `ManagedServer { ... }` construction site (there are a few — `handle_load_config` at manager.rs:513, plus any others; `cargo check` will flag each missing initializer).

**Verify:** `cargo check` compiles once all construction sites are updated.

### Step 2: Add the probe constant and threshold
Near the top of `manager.rs` (or reuse Part B's constant):

```rust
/// Timeout for a single liveness probe of a Ready MCP client.
const HEALTH_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Consecutive failed probes before a Ready server is evicted as hung.
const MAX_PROBE_FAILURES: u8 = 2;
```

**Verify:** `cargo check` compiles.

### Step 3: Extend `run_health_checks` with an active probe pass
Augment `run_health_checks` (manager.rs:1197). Keep the existing `is_closed()` pass exactly as-is; **add** a probe pass in front of it that promotes hung-but-open clients into the same `failed_ids` treatment.

Sketch (adapt names to the final code):

```rust
async fn run_health_checks(&mut self) {
    // --- Pass 1 (NEW): active liveness probe for Ready servers ---
    // Clone Arc handles, drop the read guard before any await (Part A rule).
    let probe_targets: Vec<(String, Arc<McpClient>)> = {
        let clients = self.shared_mcp_clients.read().await;
        self.servers
            .iter()
            .filter(|(_, s)| matches!(s.status, ServerStatus::Ready { .. }))
            .filter_map(|(id, _)| {
                clients.get(id).map(|c| (id.clone(), Arc::clone(c)))
            })
            .collect()
    };

    // Probe concurrently, each bounded by a timeout.
    let probe_results = futures::future::join_all(
        probe_targets.into_iter().map(|(id, client)| async move {
            let ok = matches!(
                tokio::time::timeout(HEALTH_PROBE_TIMEOUT, client.list_all_tools()).await,
                Ok(Ok(_))
            );
            (id, ok)
        }),
    )
    .await;

    // Update per-server failure counters; collect newly-hung servers.
    let mut hung_ids: Vec<String> = Vec::new();
    for (id, ok) in probe_results {
        if let Some(server) = self.servers.get_mut(&id) {
            if ok {
                server.probe_failures = 0;
            } else {
                server.probe_failures = server.probe_failures.saturating_add(1);
                if server.probe_failures >= MAX_PROBE_FAILURES {
                    hung_ids.push(id.clone());
                }
            }
        }
    }

    for id in hung_ids {
        tracing::warn!("[{}] Health check: unresponsive (hung), evicting", id);
        self.push_log(&id, "[mcpsm] Health check failed: server unresponsive".to_string());
        self.evict_and_maybe_restart(&id, "Unresponsive (detected by liveness probe)").await;
    }

    // --- Pass 2 (EXISTING): closed-connection detection, unchanged ---
    let failed_ids: Vec<String> = {
        let clients = self.shared_mcp_clients.read().await;
        self.servers
            .iter()
            .filter(|(_, s)| matches!(s.status, ServerStatus::Ready { .. }))
            .filter(|(id, _)| clients.get(id.as_str()).is_some_and(|c| c.is_closed()))
            .map(|(id, _)| id.clone())
            .collect()
    };
    for id in failed_ids {
        // ... existing body, or refactored to call evict_and_maybe_restart (Step 4) ...
    }
}
```

**Design note:** probing with `list_all_tools()` on a *healthy* server is a real round-trip every 30s per server. That is cheap and acceptable, but confirm no child treats frequent `tools/list` as costly. If any does, switch the probe to `ping` (§2.1 option 2).

**Verify:** `cargo check` + `cargo clippy --workspace`.

### Step 4: Extract the eviction body into a shared helper (refactor)
Both the new probe pass and the existing `is_closed()` pass now perform the same eviction + restart sequence. Extract it to avoid duplication:

```rust
/// Evict a Ready-but-broken server: remove client, kill child, mark Error,
/// broadcast, and auto-restart if not disabled.
async fn evict_and_maybe_restart(&mut self, id: &str, reason: &str) {
    self.shared_mcp_clients.write().await.remove(id);
    if let Some(server) = self.servers.get_mut(id) {
        if let Some(ref mut child) = server.child {
            process::stop_server(child).await;
        }
        server.child = None;
        server.tools.clear();
        server.resources.clear();
        server.resource_templates.clear();
        server.prompts.clear();           // NOTE: existing is_closed() path does not clear prompts — align intentionally.
        server.peer_info = None;
        server.probe_failures = 0;        // reset so the restarted instance starts clean
        server.status = ServerStatus::Error { message: reason.to_string() };
    }
    let _ = self.tool_change_tx.send(());
    self.sync_shared_state().await;
    send(&self.evt_tx, BackendEvent::ServerStatusChanged {
        id: id.to_string(),
        status: ServerStatus::Error { message: reason.to_string() },
    });

    let is_disabled = self.servers.get(id).is_some_and(|s| s.config.disabled);
    if !is_disabled {
        tracing::info!("[{}] Attempting auto-restart after health check failure", id);
        self.push_log(id, "[mcpsm] Attempting auto-restart...".to_string());
        self.handle_start_server(id).await;
    }
}
```

Then rewrite the existing Pass-2 loop body (manager.rs:1213-1259) to call `self.evict_and_maybe_restart(&id, "Connection lost (detected by health check)").await;`.

> **Behavior-change flag for review:** the current `is_closed()` path (manager.rs:1220-1234) clears `tools`, `resources`, `resource_templates`, `peer_info` but **not** `prompts`. The helper above clears `prompts` too. This is a small correctness improvement (stale prompts shouldn't linger for a dead server), but call it out so it's a deliberate decision, not an accidental diff.

**Verify:** `cargo check`; confirm the refactor is behavior-preserving for the `is_closed()` path except the intentional `prompts` clear.

### Step 5: Optional — restart backoff / max-restart cap (§2.4 option b)
If review chooses (b): add a `restart_attempts: u8` + `last_restart: Instant` to `ManagedServer`; in `evict_and_maybe_restart`, skip auto-restart (leave in `Error`) once attempts exceed a cap within a window, and log that manual intervention is needed. Reset the counter on a successful `Ready` transition in `handle_connect_result` (manager.rs:394).

**Verify:** simulate a server that goes Ready then immediately hangs repeatedly; confirm it stops auto-restarting after the cap and stays in `Error`.

### Step 6: `futures` dependency check
The concurrent probe uses `futures::future::join_all`. Confirm `futures` is already a dependency of `mcpsm-core` (`rg '^futures' crates/mcpsm-core/Cargo.toml`). If absent, either add it or use `tokio::task::JoinSet` (no new dependency) instead — **prefer `JoinSet`** to avoid pulling in `futures` solely for this.

**Verify:** `cargo build --workspace` with the chosen approach.

---

## 4. Verification & Safety

### 4.1 Commands
1.  `cargo check --workspace`
2.  `cargo clippy --workspace`
3.  `cargo test --workspace`
4.  `./scripts/build-app.sh`

### 4.2 Functional tests
*   **Hung-but-open child (the core case):** start a stdio child, let it reach `Ready`, then `SIGSTOP` the process (open pipe, alive, unresponsive). Confirm:
    *   After `MAX_PROBE_FAILURES` ticks (~60s), a warning + eviction log appears.
    *   The server transitions to `Error`, then auto-restarts (if not disabled).
    *   Proxy `list_tools` **stops** paying a timeout for that server once it's evicted (contrast with Part-B-only behavior, where the timeout recurs forever).
*   **Healthy server not evicted:** run a normal fast server under continuous proxy load for several minutes; confirm `probe_failures` never reaches the threshold and no spurious restart occurs.
*   **Momentary slowness:** make a child slow for one probe interval then recover; confirm one failure is recorded, then reset to 0 on recovery, no eviction.
*   **No re-introduced stall:** during probing, click Start/Stop/Restart in the dashboard; the manager loop must stay responsive (verifies §2.3 was honored).
*   **Restart-loop guard (if Step 5 done):** confirm a perpetually-hanging server stops restarting after the cap.

### 4.3 What Part C resolves
*   **Resolves:** the last remaining inefficiency from Parts A + B — an alive-but-hung child is now detected and recycled automatically instead of costing a timeout window on every proxy request indefinitely.
*   **Together with A + B:** a misbehaving child (a) never freezes the manager or dashboard, (b) never hangs the aggregate response, and (c) is automatically evicted and restarted — closing the multi-day-hang failure mode end to end.

### 4.4 Risks / open questions for review
1.  **Probe cost:** `list_all_tools()` every 30s per Ready server. Acceptable for typical setups; switch to `ping` if any child treats `tools/list` as expensive.
2.  **Probe timeout vs. request timeout:** should the health probe use a shorter `HEALTH_PROBE_TIMEOUT` (faster detection) or reuse Part B's `CHILD_REQUEST_TIMEOUT`? Recommend a dedicated shorter one.
3.  **Threshold tuning:** `MAX_PROBE_FAILURES = 2` (~60s). Too low risks evicting briefly-busy servers; too high delays recovery.
4.  **Restart backoff:** include Step 5 now or defer? Recommend deferring unless restart loops are observed.
