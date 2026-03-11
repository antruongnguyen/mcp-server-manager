# MCPSM — Product Specification

## Overview

MCPSM (MCP Server Manager) is a native macOS status bar application that manages Model Context Protocol (MCP) server instances and acts as a unified proxy. It allows developers to configure multiple MCP servers once and expose them through a single endpoint to any MCP client.

---

## Features

### F1. Unified MCP Proxy
- Single `/mcp` endpoint aggregates tools from all running child servers
- Streamable HTTP transport (POST/GET/DELETE) via rmcp SDK
- Session management handled by rmcp's `StreamableHttpService`
- Tool namespacing: `{server_id}__{tool_name}` (double underscore separator) prevents collisions
- Tool descriptions prefixed with `[server_id]` for clarity

### F2. Server Management
- **Stdio servers**: spawn child processes with command + args + env, communicate via stdin/stdout
- **Remote HTTP servers**: connect to external MCP servers via URL using `StreamableHttpClientTransport`
- Server lifecycle: `Stopped → Starting → Initializing → Ready → Stopping → Stopped`
- Error state captures failure messages from MCP handshake or process errors

### F3. Auto-Start on Launch
- Servers with `disabled: false` (default) automatically start when MCPSM launches
- Auto-start runs after config is loaded, iterating non-disabled servers sequentially
- Disabled servers are loaded but remain in Stopped state

### F4. Disabled Flag / Auto-Start Toggle
- Per-server `disabled` boolean in config (default: `false`, omitted from JSON when false via `skip_serializing_if`)
- Dashboard "Auto Start" toggle switch in server content header
- Toggling disabled: saves config only — does NOT start or stop the server
- Sidebar dims disabled servers (reduced opacity)
- Start All / Stop All buttons in header for bulk operations (conditional visibility)

### F5. Configurable Port
- Top-level `"port"` key in config JSON (default: `17532`)
- Extracted at startup before tokio runtime starts
- Passed to web server bind, status bar URL, and config save
- Non-default port persisted in config saves; default port omitted to keep config clean

### F6. Config File Watching
- `notify` crate (v7, kqueue backend on macOS) watches config file's parent directory
- Dedicated std::thread (not tokio) for the watcher event loop
- Filters for config filename only (Create + Modify events)
- 500ms debounce window to handle atomic-write editors (write-tmp-rename pattern)
- Smart reload via diff:
  - **Removed servers**: stop and remove
  - **New servers**: add, auto-start if enabled
  - **Changed config** (command/args/env/url): stop, update, restart if enabled
  - **Changed enabled only**: start or stop accordingly
  - **Unchanged**: skip
- Ignores events within 2 seconds of our own saves (`last_save_instant` tracking)

### F7. Edit Config Menu Item
- "Edit Config" in macOS status bar menu (between "Open Dashboard" and separator)
- Opens `~/.config/mcpsm/mcp.json` via `open::that()` (uses system default editor)

### F8. Rich Tool/Server Info
- **ToolInfo**: name, title, description, annotations (read_only_hint, destructive_hint, idempotent_hint, open_world_hint)
- **McpPeerInfo**: server name, version, title, description, protocol_version, instructions, capabilities
- **McpCapabilities**: tools, resources, prompts, logging (boolean flags)
- Converted from rmcp types in manager.rs (`tool_to_info()`, `server_info_to_peer_info()`)
- Dashboard renders:
  - MCP Server Info panel with name/version, protocol badge, capability badges
  - Instructions block (if server provides them)
  - Tool list with descriptions and color-coded annotation badges

### F9. Web Dashboard
- Embedded HTML (compiled into binary via `include_str!`)
- Dark/light theme with CSS custom properties, inspired by [Islands theme](https://github.com/gu-xiaohui/islands-theme)
- Theme toggle button in header (sun/moon icon), persisted in `localStorage`
- Separate `--logo-*` CSS variables for header logo visibility tuned per theme
- Sidebar: server list with status dots, tool counts, disabled dimming
- Content area: status badge, start/stop buttons, auto-start toggle, config details, MCP info, tools, logs
- Header: logo, title, Start All / Stop All buttons (conditional), Help button, theme toggle
- Modals: Add Server (with template selector), Edit Server, Confirm Delete, Proxy Help (with setup instructions)
- All URLs use `window.location.origin` (works for any port)
- SSE connection for real-time updates (status, tools, logs)

### F10. Status Bar App
- Native macOS NSStatusItem with "MCP" title
- Menu items: "Open Dashboard", "Edit Config", separator, "Quit MCPSM" (⌘Q)
- Uses OnceLock statics for cmd_tx and port (set from main.rs before NSApp run loop)

### F11. Graceful Shutdown
- Quit menu sends `AppCommand::Shutdown` via CMD_TX, then exits after 500ms
- Ctrl+C signal handler sends `AppCommand::Shutdown` via tokio signal
- Shutdown handler stops all running servers (SIGTERM → 5s → SIGKILL)

### F12. Real-Time Log Viewer
- Ring buffer: 200 lines per server (VecDeque with eviction)
- stderr from child processes captured by spawned tokio task
- Lifecycle logs injected: start command, handshake info, ready status, stop events
- Dashboard: monospaced log panel with auto-scroll, clear button

### F13. Built-In Templates
- Sequential Thinking (`@modelcontextprotocol/server-sequential-thinking`)
- Knowledge Graph Memory (`@modelcontextprotocol/server-memory`)
- Context7 (`@upstash/context7-mcp`)
- Template selector in Add Server modal auto-fills ID, command, args, env

### F14. Config Persistence
- Config path: `~/.config/mcpsm/mcp.json` (auto-created on first run)
- Atomic writes: temp file + rename
- Preserves unknown top-level JSON keys (uses `serde_json::Value` for outer document)
- Auto-saves on: add, update, delete, disabled toggle

---

## Design Decisions

### D1. Two-Thread Architecture
**Decision**: Main thread for AppKit, background thread for tokio.
**Rationale**: macOS requires AppKit operations on the main thread. Tokio's async runtime needs its own thread to avoid blocking the NSApp run loop. Communication via channels keeps the two worlds decoupled.

### D2. rmcp SDK for MCP Protocol
**Decision**: Use the official `rmcp` Rust SDK (v1.1) instead of hand-rolling JSON-RPC.
**Rationale**: Eliminates custom JSON-RPC parsing, session management, and transport code. The SDK handles the MCP handshake, tool listing, and Streamable HTTP transport (server-side via `StreamableHttpService`, client-side via stdio transport or `StreamableHttpClientTransport`).

### D3. notify Crate on Dedicated std::thread
**Decision**: Config watcher runs on a std::thread, not a tokio task.
**Rationale**: `notify::RecommendedWatcher` uses blocking OS APIs (kqueue on macOS). Running it on a tokio task would block the async executor. A dedicated thread with std::sync::mpsc is simpler and more reliable.

### D4. Debounce + Last-Save Tracking for Watcher
**Decision**: 500ms debounce + 2s ignore window after our own saves.
**Rationale**: Editors like vim and VS Code perform atomic writes (write-tmp → rename), generating multiple filesystem events. Debouncing collapses these. The last-save tracking prevents infinite loops where our save triggers a watcher event which triggers another save.

### D5. Disabled Field Defaults to False
**Decision**: `#[serde(default, skip_serializing_if = "std::ops::Not::not")]` on `ServerConfig.disabled`.
**Rationale**: Clean config — disabled field is only written when `true`. Old config files without the field default to not-disabled (auto-start). Toggling disabled only saves config; it does NOT start/stop servers, giving users explicit control over server lifecycle.

### D6. Port Extracted Before Tokio Runtime
**Decision**: Early synchronous config read in `main()` to extract port before spawning the background thread.
**Rationale**: The port must be known before starting the web server. Doing an early sync read avoids needing to pass the port asynchronously or restart the server after config load.

### D7. OnceLock Statics for Status Bar Communication
**Decision**: Use `OnceLock<UnboundedSender<AppCommand>>` and `OnceLock<u16>` for the GUI → backend channel.
**Rationale**: The status bar runs on the main thread (NSApp run loop) where we can't easily pass Rust closures. OnceLock provides safe, once-initialized global access from objc2 selector methods.

### D8. Smart Config Reload (Diff-Based)
**Decision**: On watcher trigger, diff current state against newly loaded config rather than stop-all/reload-all.
**Rationale**: Avoids unnecessary server restarts. A user changing one server's args shouldn't interrupt other running servers. The diff identifies exactly what changed and takes minimal action.

### D9. Embedded Dashboard HTML
**Decision**: Single HTML file compiled into binary via `include_str!`.
**Rationale**: Zero external dependencies for the web UI. No static file serving, no build step, no file path issues. The entire dashboard is self-contained in one file with inline CSS and JS.

### D10. Tool Info Stored as Custom Types (Not rmcp Types)
**Decision**: Define `ToolInfo`, `McpPeerInfo`, etc. in `core/server.rs` and convert from rmcp types.
**Rationale**: Decouples our serialization format from rmcp's internal types. Our types derive `Serialize` for the web API and `Deserialize` for future use, without depending on rmcp's `#[non_exhaustive]` constraints. Also lets us flatten/simplify the structure for the dashboard.

---

## Non-Functional Requirements

### NFR1. Performance
- Server start/stop operations are async and non-blocking
- Shared state uses `Arc<RwLock<>>` — reads don't block each other
- Log buffer is bounded (200 lines) to prevent memory growth
- SSE broadcast channel (256 capacity) with lagged-subscriber skip

### NFR2. Reliability
- Atomic config writes prevent corruption on crash
- Process kill_on_drop ensures child cleanup if the manager panics
- Graceful shutdown stops all servers before exit
- Error states are recoverable — failed servers can be restarted

### NFR3. Backward Compatibility
- Config files without `disabled` field work (defaults to `false` = auto-start)
- Old `enabled` field is silently ignored by serde (unknown fields are skipped)
- Config files without `port` field work (defaults to `17532`)
- Unknown top-level JSON keys in config are preserved across saves

### NFR4. Security
- Binds to `127.0.0.1` only (not `0.0.0.0`) — local access only
- No authentication (local-only design assumption)
- Child process environment is explicitly controlled (only configured env vars + augmented PATH)

### NFR5. Observability
- Structured logging via `tracing` crate (INFO level by default, configurable via `RUST_LOG`)
- Per-server log buffer with lifecycle events ([mcpsm] prefixed)
- SSE event stream for real-time dashboard updates
- MCP peer info (server name, version, protocol) displayed in dashboard

### NFR6. Build
- 0 warnings in debug and release builds
- 9 unit tests (config roundtrip, log buffer, templates)
- Single binary output — no runtime dependencies beyond system frameworks

---

## Limitations

### L1. macOS Only
- Native AppKit status bar (objc2) — no Windows/Linux support
- Requires MainThreadMarker for GUI operations

### L2. No Authentication
- Dashboard and API are accessible to any local process on the machine
- Suitable for development use; not recommended for shared/production servers

### L3. No MCP Resources or Prompts Proxy
- Only tool aggregation is implemented in the proxy
- Resources, prompts, and completions from child servers are not forwarded
- The proxy advertises `tools` capability only

### L4. No Tool Change Forwarding from Children
- If a child server's tools change at runtime (via `notifications/tools/list_changed`), MCPSM does not currently detect this
- Tools are captured once at connection time (`tools/list` after handshake)
- Workaround: restart the server to refresh tools

### L5. Sequential Auto-Start
- Enabled servers are started one at a time on launch (not parallel)
- For many servers, startup can take time as each MCP handshake completes sequentially

### L6. Port Requires Restart
- Changing the `port` in config requires restarting MCPSM
- The config watcher does not reload port changes (port is bound at startup)

### L7. No Config Validation UI
- Config edits (via "Edit Config") are in raw JSON
- Invalid JSON or malformed config results in error logs, not user-facing error messages in the dashboard

### L8. Single Config File
- All servers are managed in one file (`~/.config/mcpsm/mcp.json`)
- No support for per-project or workspace-scoped configs

---

## Technical Stack

| Component | Technology | Version |
|-----------|-----------|---------|
| Language | Rust | Edition 2024 |
| macOS GUI | objc2 + objc2-foundation + objc2-app-kit | 0.6 / 0.3 |
| Async runtime | tokio | 1 (full features) |
| Web framework | axum | 0.8 |
| MCP SDK | rmcp | 1.1 |
| File watching | notify | 7 (macos_kqueue) |
| Serialization | serde + serde_json | 1 |
| Logging | tracing + tracing-subscriber | 0.1 / 0.3 |
| HTTP client | reqwest (via rmcp) | 0.13 |
| Platform dirs | dirs | 6 |
| Process mgmt | libc (SIGTERM/SIGKILL) | 0.2 |
| Open files | open | 5 |
| Error handling | anyhow | 1 |

---

## REST API Reference

### Management Endpoints

| Method | Path | Request Body | Response | Description |
|--------|------|-------------|----------|-------------|
| GET | `/` | — | HTML | Web dashboard |
| GET | `/api/servers` | — | JSON map | All servers with config, status, tools, disabled, peer_info |
| POST | `/api/servers` | `{ id, config }` | 202 | Add a new server |
| POST | `/api/servers/:id/start` | — | 202 | Start a server |
| POST | `/api/servers/:id/stop` | — | 202 | Stop a server |
| POST | `/api/servers/:id/disabled` | `{ disabled: bool }` | 202 | Set disabled flag (config only, no start/stop) |
| PUT | `/api/servers/:id` | `{ config }` | 202 | Update server config |
| DELETE | `/api/servers/:id` | — | 202 | Delete a server |
| GET | `/api/servers/:id/logs` | — | 202 | Request log snapshot (delivered via SSE) |
| DELETE | `/api/servers/:id/logs` | — | 202 | Clear server logs |
| POST | `/api/servers/start-all` | — | 202 | Start all non-disabled servers |
| POST | `/api/servers/stop-all` | — | 202 | Stop all running servers |
| GET | `/api/templates` | — | JSON array | Built-in server templates |
| GET | `/api/events` | — | SSE stream | Real-time backend events |

### MCP Proxy Endpoint

| Method | Path | Headers | Description |
|--------|------|---------|-------------|
| POST | `/mcp` | `Mcp-Session-Id` (after init) | JSON-RPC requests |
| GET | `/mcp` | `Mcp-Session-Id` | SSE notification stream |
| DELETE | `/mcp` | `Mcp-Session-Id` | Session teardown |

### SSE Event Types

| Event Type | Fields | Description |
|-----------|--------|-------------|
| `ServerStatusChanged` | `id`, `status` | Server status transition |
| `McpToolsChanged` | `id`, `tools[]` | Tool list updated (rich ToolInfo objects) |
| `McpServerReady` | `id` | Server completed MCP handshake |
| `LogLine` | `id`, `line` | Single log line from server |
| `LogSnapshot` | `id`, `lines[]` | Full log buffer snapshot |
| `ConfigLoaded` | `servers[]` | Config (re)loaded from disk |
| `Error` | `message` | Backend error message |

---

## Data Models

### ServerConfig
```
command: Option<String>     # Command to run (stdio servers)
args: Vec<String>           # Command arguments
env: HashMap<String,String> # Environment variables
url: Option<String>         # Remote server URL (HTTP servers)
auth_header: Option<String> # HTTP Authorization header (optional)
disabled: bool              # Skip auto-start (default: false, omitted when false)
```

### ServerInfo (web API response)
```
config: ServerConfig
status: ServerStatus        # Stopped|Starting|Initializing|Ready|Stopping|Error
tools: Vec<ToolInfo>
disabled: bool
peer_info: Option<McpPeerInfo>
```

### ToolInfo
```
name: String
title: Option<String>
description: Option<String>
annotations: Option<ToolAnnotationInfo>
  read_only_hint: Option<bool>
  destructive_hint: Option<bool>
  idempotent_hint: Option<bool>
  open_world_hint: Option<bool>
```

### McpPeerInfo
```
name: String                # MCP server implementation name
version: String             # MCP server implementation version
title: Option<String>
description: Option<String>
protocol_version: String    # e.g., "2025-06-18"
instructions: Option<String>
capabilities: McpCapabilities
  tools: bool
  resources: bool
  prompts: bool
  logging: bool
```

---

## TODO / Future Work

- [ ] **Parallel auto-start**: start enabled servers concurrently instead of sequentially
- [ ] **Dynamic tool refresh**: listen for `notifications/tools/list_changed` from child servers and re-fetch tools
- [ ] **Resource/prompt proxy**: forward resources and prompts from child servers, not just tools
- [ ] **Config validation**: validate config structure and show user-friendly errors in dashboard
- [ ] **Per-project configs**: support workspace-scoped server configs in addition to global
- [ ] **Server health checks**: periodic ping to detect crashed servers and auto-restart
- [ ] **Server grouping/tags**: organize servers into groups in the dashboard
- [ ] **Tool search/filter**: search bar in dashboard to filter tools across servers
- [ ] **Export/import config**: share server configurations between machines
- [ ] **Authentication**: optional auth for the web dashboard and API
- [ ] **Cross-platform**: support Linux/Windows (replace objc2 with cross-platform tray library)
- [ ] **Tool usage analytics**: track which tools are called and how often
- [ ] **Log persistence**: optionally write logs to disk for post-mortem debugging
- [ ] **Custom tool annotations**: allow user-defined annotations on proxied tools
- [ ] **Hot port reload**: detect port change in config and rebind without full restart
