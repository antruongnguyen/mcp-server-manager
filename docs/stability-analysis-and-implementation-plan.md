# MCPSM Long-Running Stability Analysis & Implementation Plan

> **Status:** Analysis verified against source (2026-07-10). Diagnosis confirmed accurate.
> Fix is approved in principle, with corrections to framing (below) and **one required
> addition** — per-child request timeouts — before the multi-day-hang symptom can be
> considered resolved. Awaiting owner review/decision before implementation.

## 1. Executive Summary
This document investigates a stability issue in `mcpsm` where the server stops responding, dashboard controls stop working, and the application becomes unresponsive after running for extended periods (typically a few days).

The root cause is **read guards on a shared `tokio::sync::RwLock` being held across network/process `.await` boundaries** inside the MCP proxy handler. When a child MCP server becomes slow or unresponsive, the proxy task keeps its read guard alive across the hung `.await`, which starves the background `ServerManager` of the write access it needs during state synchronization. Because the manager runs as a single-threaded `select!` loop, a blocked write stalls the entire loop — no commands, log lines, or state updates are processed — and every dashboard API read queued behind the pending write also stalls. Restarting the app is the only recovery.

**Two important qualifications** (see §2.4 and §3.3):
1. This is a **lock-contention stall driven by a hung child `.await`**, not a classic lock-ordering deadlock. It self-heals if the child ever responds; it becomes permanent only because a hung child never does.
2. The scoped-lock refactor in §4 removes the **collateral damage** (frozen manager + frozen dashboard) but does **not** remove the **trigger** (a hung child with no timeout). The proxy's own `/mcp` tool-listing request will still hang on the dead child until a per-child timeout is added (§4 Step 6). Both parts are needed to call the multi-day-hang issue resolved.

---

## 2. Root Cause Analysis

### 2.1 The Shared State Architecture
In `mcpsm-core`, server configurations, status, and running instances are shared across layers (web server, MCP proxy, and background `ServerManager`) using thread-safe containers wrapped in `Arc<RwLock<...>>` (defined in `crates/mcpsm-core/src/core/manager.rs:33-36`, wired together in `crates/mcpsm-core/src/runtime.rs:25-53`):
*   `self.servers` (`SharedServers`): `Arc<RwLock<HashMap<String, ServerInfo>>>`
*   `self.clients` (`SharedMcpClients`): `Arc<RwLock<HashMap<String, Arc<McpClient>>>>`

The **same `Arc<RwLock>` instances** are shared by the proxy (`ProxyHandler`), the web layer (`AppState`), and the manager — confirmed in `runtime.rs`.

### 2.2 The Vulnerability
In `crates/mcpsm-core/src/mcp/proxy.rs`, the proxy aggregates tools, resources, and prompts from all active children. The listing methods acquire read locks and then hold them across an async child query:

```rust
async fn list_tools(
    &self,
    _request: Option<PaginatedRequestParams>,
    _context: RequestContext<RoleServer>,
) -> Result<ListToolsResult, McpError> {
    // 1. Acquire read locks on clients and servers maps (proxy.rs:58-59)
    let clients = self.clients.read().await;
    let servers = self.servers.read().await;

    let mut all_tools: Vec<Tool> = Vec::new();

    for (server_id, client) in clients.iter() {
        ...
        // ❌ CRITICAL: async .await while the read guards above are still in scope.
        //    They are dropped only at end-of-function.
        match client.list_all_tools().await {   // proxy.rs:73
            Ok(tools) => { ... }
            Err(e) => { ... }
        }
    }
    ...
}
```

Because `clients` and `servers` are `RwLockReadGuard`s, they remain alive in the task's environment until `list_tools` returns.

### 2.3 The Contention Chain
When an external LLM client connects to the proxy and queries `/mcp` (tools, resources, or prompts):

1.  **Read locks acquired**: The proxy task acquires read locks on both `self.clients` and `self.servers`.
2.  **Async await**: The task begins querying individual child servers over stdio/HTTP via `client.list_all_tools().await`. The task yields to the executor, but **the read guards are still held**.
3.  **Hung child server**: If a child becomes slow, sleeps, or hangs (process exhaustion, network latency, system idle over days), the `.await` blocks — and there is **no timeout** on this call (see §3.3), so it can block indefinitely.
4.  **Write lock request**: Concurrently, the background `ServerManager` handles an event (log line, health-check tick, or a user toggle) and triggers a state sync:
    ```rust
    // manager.rs:1598-1622
    async fn sync_shared_state(&self) {
        let snapshot: HashMap<String, ServerInfo> = /* clone of current state */;
        // ❌ Attempts to acquire a Write Lock on the shared servers map.
        *self.shared.write().await = snapshot;
    }
    ```
5.  **Write blocks behind the held read guard**:
    *   The proxy task holds an active read guard, so `ServerManager`'s write request cannot proceed and is queued.
    *   Tokio's `RwLock` acquires through a fair (FIFO-ish) semaphore. Once a writer is queued, subsequent readers generally queue behind it as well — so the dashboard's read requests also stall. (Note: this is *fair queuing behind a pending writer*, **not** a hard "write-preferring" guarantee — see §2.4.)
6.  **Application freeze**:
    *   `ServerManager::run()` (manager.rs:249-292) is a single `tokio::select!` loop, and its handlers call `sync_shared_state().await` **inline**. Blocking on that write freezes the entire loop: no more `cmd_rx` commands, log lines, or connect/refresh results are processed.
    *   When the dashboard tries `/api/servers`, the handler does `state.servers.read().await` (`handlers.rs:25`). Queued behind the pending write, the request stalls indefinitely, freezing the web UI.
    *   Start / Stop / Restart do nothing because the manager can no longer pull from the command queue.

This explains why restarting `mcpsm` is required to recover.

### 2.4 Correction: "deadlock" vs. "stall", and the `RwLock` fairness claim
Two framing points from earlier drafts of this analysis were overstated and are corrected here:

*   **Not a classic deadlock.** There is no lock-ordering cycle between two tasks each holding a lock the other needs. It is a single task holding a read guard across a hung `.await`, starving a writer. It would **self-heal** the instant the child responds; it only becomes permanent because a hung child never responds. Accurate label: **lock-contention stall triggered by an unbounded child `.await`.**
*   **Tokio `RwLock` is not strictly "write-preferring".** It does not guarantee writer priority. It uses an internal semaphore with fair acquisition, so in practice readers queued *after* a pending writer wait behind it. The stall argument does **not** depend on a write-preference guarantee — a held read guard blocks the queued writer directly, which is sufficient.

Neither correction changes the conclusion; they make the analysis defensible under scrutiny.

---

## 3. Solution Strategy

### 3.1 Part A — Minimize Lock Contention via Cloned Scopes
Release all read guards **before** any `.await`. Acquire the read locks briefly, clone the (cheap) contents into local variables inside a synchronous scope, and drop the guards immediately:
*   `Arc<McpClient>` clone = one atomic refcount bump.
*   `ServerInfo` derives `Clone` and holds small collections.

This removes the collateral damage: the manager loop and dashboard can no longer be frozen by a hung child, because no proxy request holds a shared lock across a network await.

### 3.2 Affected Methods to Refactor (Part A)
Apply scoped cloning to the five methods in `crates/mcpsm-core/src/mcp/proxy.rs` that currently hold guards across awaits:
1.  `list_tools` (proxy.rs:53)
2.  `list_resources` (proxy.rs:156)
3.  `list_resource_templates` (proxy.rs:214)
4.  `list_prompts` (proxy.rs:322)
5.  `set_level` (proxy.rs:418)

**Already correct — do not touch:** `call_tool` (proxy.rs:107), `read_resource` (proxy.rs:278), and `get_prompt` (proxy.rs:369) already scope the lock in a small block, clone the `Arc<McpClient>`, and drop the guard before awaiting the child. They are the reference pattern for this fix.

> Note on `set_level`: it reads `servers` only for capability gating. Cloning the `servers` map for it is the cheapest of the five; kept for consistency.

### 3.3 Part B — Bound Every Child Request with a Timeout (REQUIRED)
Part A alone is **not sufficient** to resolve the reported symptom. Verified in `crates/mcpsm-core/src/mcp/client.rs:193-196`:

```rust
pub async fn list_tools(client: &McpClient) -> anyhow::Result<Vec<Tool>> {
    let tools = client.list_all_tools().await?;   // no timeout anywhere
    Ok(tools)
}
```

There is no `set_request_timeout`, no `tokio::time::timeout`, no cancellation deadline on any child call. After Part A, a hung child no longer freezes the manager or dashboard — a real and worthwhile win — **but the proxy's own `/mcp` request still hangs forever** waiting on the dead child. The external LLM client's `list_tools` call (the thing the user actually cares about) still stalls.

The **trigger** described in §2.3 step 3 ("child hangs after a few days") remains unaddressed without a bounded wait. Part B wraps each per-child `.await` in `tokio::time::timeout(...)`; on elapse it logs and skips that child, so one dead server degrades gracefully instead of hanging the aggregate response.

This turns "recovers only on app restart" into "one slow server is skipped, everything else works."

### 3.4 Optional Part C — Evict Unresponsive Clients (future hardening)
A follow-up (not required for this fix) is to have the health-check tick (manager.rs:288) detect repeatedly-timing-out clients and evict/restart them, so a permanently-dead child stops being queried at all rather than costing one timeout per proxy request. Listed for completeness; can be deferred.

---

## 4. Implementation Plan

Each step lists a verification check. Steps 1–5 are the Part A refactor; Step 6 is Part B; Step 7 is verification.

### Step 1: Refactor `list_tools`
Release the read locks by cloning the maps inside a scoped block before any `.await`.

```rust
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        // Scoped block: acquire, clone, drop guards before any async .await.
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut all_tools: Vec<Tool> = Vec::new();

        for (server_id, client) in &clients {
            let is_ready = servers
                .get(server_id)
                .is_some_and(|s| matches!(s.status, ServerStatus::Ready { .. }));

            if !is_ready {
                continue;
            }

            // Part B (Step 6) wraps this await in a timeout.
            match client.list_all_tools().await {
                Ok(tools) => {
                    for tool in tools {
                        let namespaced_name: Cow<'static, str> =
                            format!("{}{}{}", server_id, NAMESPACE_SEP, tool.name).into();
                        let description: Option<Cow<'static, str>> = tool
                            .description
                            .map(|d| format!("[{}] {}", server_id, d).into());
                        let mut namespaced_tool =
                            Tool::new_with_raw(namespaced_name, description, tool.input_schema);
                        if let Some(annotations) = tool.annotations {
                            namespaced_tool = namespaced_tool.with_annotations(annotations);
                        }
                        all_tools.push(namespaced_tool);
                    }
                }
                Err(e) => {
                    tracing::warn!("[{}] Failed to list tools in proxy: {}", server_id, e);
                }
            }
        }

        Ok(ListToolsResult { tools: all_tools, next_cursor: None, meta: None })
    }
```
**Verify:** `cargo check` compiles; grep confirms no `RwLockReadGuard` is alive across the `list_all_tools().await`.

### Step 2: Refactor `list_resources`
Same scoped-locking structure.

```rust
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut all_resources: Vec<Resource> = Vec::new();

        for (server_id, client) in &clients {
            let has_resources = servers.get(server_id).is_some_and(|s| {
                matches!(s.status, ServerStatus::Ready { .. })
                    && s.peer_info.as_ref().is_some_and(|p| p.capabilities.resources)
            });

            if !has_resources {
                continue;
            }

            match client.list_all_resources().await {
                Ok(resources) => {
                    for resource in resources {
                        let namespaced_uri =
                            format!("{}{}{}", server_id, NAMESPACE_SEP, resource.raw.uri);
                        let description = resource
                            .raw
                            .description
                            .map(|d| format!("[{}] {}", server_id, d));
                        let mut raw = RawResource::new(namespaced_uri, resource.raw.name);
                        if let Some(title) = resource.raw.title {
                            raw = raw.with_title(title);
                        }
                        if let Some(desc) = description {
                            raw = raw.with_description(desc);
                        }
                        if let Some(mime) = resource.raw.mime_type {
                            raw = raw.with_mime_type(mime);
                        }
                        all_resources.push(Annotated::new(raw, resource.annotations));
                    }
                }
                Err(e) => {
                    tracing::warn!("[{}] Failed to list resources in proxy: {}", server_id, e);
                }
            }
        }

        Ok(ListResourcesResult { resources: all_resources, next_cursor: None, meta: None })
    }
```
**Verify:** `cargo check` compiles.

### Step 3: Refactor `list_resource_templates`
```rust
    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut all_templates: Vec<ResourceTemplate> = Vec::new();

        for (server_id, client) in &clients {
            let has_resources = servers.get(server_id).is_some_and(|s| {
                matches!(s.status, ServerStatus::Ready { .. })
                    && s.peer_info.as_ref().is_some_and(|p| p.capabilities.resources)
            });

            if !has_resources {
                continue;
            }

            match client.list_all_resource_templates().await {
                Ok(templates) => {
                    for template in templates {
                        let namespaced_uri = format!(
                            "{}{}{}",
                            server_id, NAMESPACE_SEP, template.raw.uri_template
                        );
                        let description = template
                            .raw
                            .description
                            .map(|d| format!("[{}] {}", server_id, d));
                        let mut raw =
                            RawResourceTemplate::new(namespaced_uri, template.raw.name);
                        if let Some(title) = template.raw.title {
                            raw = raw.with_title(title);
                        }
                        if let Some(desc) = description {
                            raw = raw.with_description(desc);
                        }
                        if let Some(mime) = template.raw.mime_type {
                            raw = raw.with_mime_type(mime);
                        }
                        all_templates.push(Annotated::new(raw, template.annotations));
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[{}] Failed to list resource templates in proxy: {}",
                        server_id,
                        e
                    );
                }
            }
        }

        Ok(ListResourceTemplatesResult {
            resource_templates: all_templates,
            next_cursor: None,
            meta: None,
        })
    }
```
**Verify:** `cargo check` compiles.

### Step 4: Refactor `list_prompts`
```rust
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut all_prompts: Vec<Prompt> = Vec::new();

        for (server_id, client) in &clients {
            let has_prompts = servers.get(server_id).is_some_and(|s| {
                matches!(s.status, ServerStatus::Ready { .. })
                    && s.peer_info.as_ref().is_some_and(|p| p.capabilities.prompts)
            });

            if !has_prompts {
                continue;
            }

            match client.list_all_prompts().await {
                Ok(prompts) => {
                    for prompt in prompts {
                        let namespaced = Prompt::new(
                            format!("{}{}{}", server_id, NAMESPACE_SEP, prompt.name),
                            prompt.description.map(|d| format!("[{}] {}", server_id, d)),
                            prompt.arguments,
                        );
                        all_prompts.push(namespaced);
                    }
                }
                Err(e) => {
                    tracing::warn!("[{}] Failed to list prompts in proxy: {}", server_id, e);
                }
            }
        }

        Ok(ListPromptsResult { prompts: all_prompts, next_cursor: None, meta: None })
    }
```
**Verify:** `cargo check` compiles.

### Step 5: Refactor `set_level`
```rust
    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut any_success = false;
        let mut last_error: Option<String> = None;

        for (server_id, client) in &clients {
            let has_logging = servers.get(server_id).is_some_and(|s| {
                matches!(s.status, ServerStatus::Ready { .. })
                    && s.peer_info.as_ref().is_some_and(|p| p.capabilities.logging)
            });

            if !has_logging {
                continue;
            }

            let params = SetLevelRequestParams::new(request.level.clone());
            match client.set_level(params).await {
                Ok(()) => any_success = true,
                Err(e) => {
                    tracing::warn!("[{}] Failed to set log level in proxy: {}", server_id, e);
                    last_error = Some(format!("{}: {}", server_id, e));
                }
            }
        }

        if any_success {
            Ok(())
        } else if let Some(err) = last_error {
            Err(McpError::internal_error(format!("Failed to set log level: {}", err), None))
        } else {
            Ok(())
        }
    }
```
**Verify:** `cargo check` compiles.

### Step 6: Bound every child request with a timeout (Part B — REQUIRED)
Wrap each per-child `.await` in the five refactored methods (and, for completeness, in `call_tool` / `read_resource` / `get_prompt`) with `tokio::time::timeout`. On elapse, log and skip that child rather than hanging the whole aggregate response.

Introduce a single constant and apply it uniformly. Example for the `list_tools` inner call:

```rust
// Near the top of proxy.rs
use std::time::Duration;
const CHILD_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
```

```rust
            // Inside the loop, replacing the bare `client.list_all_tools().await`:
            match tokio::time::timeout(CHILD_REQUEST_TIMEOUT, client.list_all_tools()).await {
                Ok(Ok(tools)) => { /* existing Ok(tools) arm */ }
                Ok(Err(e)) => {
                    tracing::warn!("[{}] Failed to list tools in proxy: {}", server_id, e);
                }
                Err(_elapsed) => {
                    tracing::warn!(
                        "[{}] list_tools timed out after {:?}; skipping",
                        server_id,
                        CHILD_REQUEST_TIMEOUT
                    );
                }
            }
```

Apply the same `timeout(...)` wrapping to:
*   `list_resources` → `client.list_all_resources()`
*   `list_resource_templates` → `client.list_all_resource_templates()`
*   `list_prompts` → `client.list_all_prompts()`
*   `set_level` → `client.set_level(params)`
*   `call_tool` → `client.call_tool(child_params)` (return an error `CallToolResult` on elapse)
*   `read_resource` → `client.read_resource(child_params)`
*   `get_prompt` → `client.get_prompt(child_params)`

**Design decisions to confirm during review:**
*   **Timeout value:** 30s is a starting point. It should exceed any legitimate slow child but be short enough that a user notices degradation rather than a hang. Make it a constant now; consider config-driven later.
*   **Per-child vs. whole-request timeout:** per-child (recommended) means one slow server can't consume the whole budget and starve others. A single dead child costs one timeout window per proxy request until Part C evicts it.
*   **Location:** wrapping at the proxy call sites (shown above) is the minimal-surface change. Alternatively, set an rmcp request timeout at client construction in `client.rs` — cleaner but broader; deferred unless preferred.

**Verify:** `cargo check` + a manual test where a child is `SIGSTOP`-ed (or points at an unresponsive endpoint): the proxy `list_tools` returns the other servers' tools within ~30s and logs the skip, instead of hanging.

### Step 7: Regression test for the lock contention (recommended)
CodeGraph confirms none of the five methods, nor `sync_shared_state`, currently have covering tests — nothing prevents a future edit from reintroducing a guard-across-await. Add a focused async test that:
1.  Builds a `ProxyHandler` over shared maps containing one client that blocks indefinitely on `list_all_tools` (a stub/mock).
2.  Spawns the proxy `list_tools` call.
3.  From another task, acquires `servers.write().await` (simulating `sync_shared_state`) and asserts it completes within a short timeout.

Before the fix this write would stall; after it, it proceeds immediately. This is the guardrail that locks the fix in place.

**Verify:** the new test fails on the pre-fix code and passes on the post-fix code.

---

## 5. Verification & Safety Measures

### 5.1 Verification Commands
1.  **Code check:** `cargo check --workspace`
2.  **Lint:** `cargo clippy --workspace`
3.  **Unit tests:** `cargo test --workspace` (existing 9 tests + new regression test from Step 7)
4.  **Rebuild bundle:** `./scripts/build-app.sh`

### 5.2 Functional Verification Strategy
*   Start the recompiled `MCPSM.app`.
*   Launch multiple configured stdio and HTTP servers.
*   **Contention test (Part A):** issue continuous proxy `list_tools` requests (curl loop or Claude Code / Gemini CLI) while clicking Start/Stop/Restart and toggling enable/disable in the dashboard. Controls must respond instantly and the manager loop must keep processing commands.
*   **Timeout test (Part B):** make one child unresponsive (`SIGSTOP` the process, or point an HTTP server at a black-hole endpoint). Confirm:
    *   The proxy `list_tools`/`list_resources`/`list_prompts` responses return within the timeout window, containing the *other* servers' entries.
    *   A skip warning is logged for the unresponsive child.
    *   The dashboard and manager remain fully responsive throughout.
*   **Long-run sanity:** leave it running with an intentionally flaky child and confirm no accumulating stall over time.

### 5.3 What this plan does and does not resolve
*   **Resolves:** the freeze cascade — a hung child can no longer freeze the manager loop or the dashboard (Part A), and can no longer hang the aggregate proxy response (Part B).
*   **Does not resolve (deferred to optional Part C):** a permanently-dead child is still *queried* on every proxy request, costing one timeout window each time until health-check-driven eviction is added. This is graceful degradation, not elimination of the wasted work.
