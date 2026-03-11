# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
cargo build              # Debug build
cargo build --release    # Release build
cargo run                # Run the app (must be on macOS)
cargo test               # Run all 9 unit tests
cargo test config        # Run config tests only
cargo test log_buffer    # Run log buffer tests only
cargo test templates     # Run template tests only
```

## Architecture

Native macOS status bar app in Rust. Two-thread design with a channel bridge:

- **Main thread**: AppKit status bar (NSStatusItem via objc2)
- **Background thread**: tokio async runtime (ServerManager + axum web server + MCP proxy)

Communication:
- Web/GUI → Backend: `tokio::sync::mpsc::unbounded_channel<AppCommand>`
- Backend → Web: `tokio::sync::broadcast::channel<BackendEvent>` (SSE streaming)
- Shared state: `Arc<RwLock<HashMap<String, ServerInfo>>>` for web layer reads
- Shared MCP clients: `Arc<RwLock<HashMap<String, Arc<McpClient>>>>` for proxy access
- Tool changes: `tokio::sync::watch::channel<()>` signals proxy when tool lists change
- Config watcher: `notify` crate on a dedicated std::thread, sends `ReloadConfigIfChanged` through cmd channel

The bridge contract lives in `src/bridge/commands.rs` — all interaction goes through `AppCommand` and `BackendEvent` enums.

## Module Layout

- **`src/core/`** — Business logic: config I/O, process spawn/kill, log ring buffer, server manager async loop, config file watcher
- **`src/bridge/`** — Channel contract: `AppCommand` + `BackendEvent` enums
- **`src/gui/`** — macOS status bar ("Open Dashboard" + "Edit Config" + "Quit" with graceful shutdown)
- **`src/mcp/`** — MCP protocol via rmcp SDK: client wrapper (stdio + HTTP), proxy handler (tool aggregation + routing)
- **`src/web/`** — axum web server: REST API, SSE, dashboard HTML, MCP endpoint

## Dashboard Theme System

The dashboard uses CSS custom properties with `[data-theme="dark"]` / `[data-theme="light"]` selectors, inspired by the Islands VS Code theme. Theme is applied before first paint via inline `<script>` in `<head>` and persisted in `localStorage` as `mcpsm-theme`.

Key color variable groups:
- `--bg`, `--surface`, `--surface2`, `--border` — layer hierarchy
- `--text`, `--text2` — primary/secondary text
- `--accent`, `--green`, `--yellow`, `--red`, `--blue` — semantic colors
- `--green-bg`, `--yellow-bg`, `--red-bg`, `--blue-bg` — tinted badge backgrounds
- `--logo-*` — separate logo-specific colors tuned per theme for header visibility

## MCP Proxy

MCPSM acts as a unified MCP proxy server at `http://127.0.0.1:{port}/mcp` (port configurable, default 17532):

- **Child communication**: rmcp SDK over stdin/stdout (stdio) or StreamableHttpClientTransport (HTTP)
- **McpClient** (`mcp/client.rs`): type alias for `RunningService<RoleClient, ClientInfo>`, connect_stdio/connect_http wrappers
- **ProxyHandler** (`mcp/proxy.rs`): implements `rmcp::handler::server::ServerHandler`, tool aggregation with namespacing (`server_id__tool_name`)
- **Transport**: rmcp's `StreamableHttpService` mounted via `.nest_service("/mcp", ...)`
- **Server lifecycle**: Stopped → Starting → Initializing (MCP handshake) → Ready (tools discovered)

## Config File

Reads/writes `~/.config/mcpsm/mcp.json` (auto-created on first run). Uses `serde_json::Value` for the outer document to **preserve unknown fields** — only the `mcpServers` and `port` keys are modified. Writes are atomic (temp file + rename).

Config supports:
- `port` (u16, default 17532) — extracted at startup, persisted if non-default
- `mcpServers` — map of server configs with `command`/`args`/`env` (stdio) or `url` (HTTP) + `enabled` (default true)

Config file watcher (`src/core/watcher.rs`) uses `notify` crate (kqueue on macOS) with 500ms debounce. Smart reload diffs old vs new config: stops removed servers, adds new ones (auto-starts if enabled), restarts changed ones. Ignores events within 2s of our own saves.

## Server Enabled/Disabled

`ServerConfig.enabled` field (`#[serde(default = "default_true")]`). Enabled servers auto-start on launch. The `SetServerEnabled` command updates config, saves, and starts/stops the server accordingly. Dashboard shows a toggle switch.

## Rich Tool/Server Info

- `ToolInfo`: name, title, description, annotations (read_only_hint, destructive_hint, idempotent_hint, open_world_hint)
- `McpPeerInfo`: name, version, title, description, protocol_version, instructions, capabilities
- Converted from rmcp types (`Tool` → `ToolInfo`, `ServerInfo/Implementation` → `McpPeerInfo`) in manager.rs
- Dashboard renders tool descriptions, annotation badges, and MCP server info panel with capability badges

## Process Management

Servers are spawned via `tokio::process::Command` with piped stdin/stdout/stderr:
- **stdin/stdout** — reserved for MCP JSON-RPC protocol communication (rmcp SDK)
- **stderr** — captured as log lines by a tokio task, fed into the manager's `select!` loop

PATH is augmented with `/usr/local/bin`, `/opt/homebrew/bin` for `npx` discovery. Graceful shutdown: SIGTERM → 5s wait → SIGKILL.

## Status Bar Communication

- `static CMD_TX: OnceLock<UnboundedSender<AppCommand>>` — set from main.rs, used by Quit to send Shutdown
- `static DASHBOARD_PORT: OnceLock<u16>` — set from main.rs, used for dynamic dashboard URL
- Ctrl+C signal handler also sends `AppCommand::Shutdown`

## objc2 Patterns (Critical)

This project uses **objc2 0.6** with **edition 2024**. Key patterns:

- `define_class!` requires: separate ivars struct, `#[ivars = IvarStruct]` attribute, `struct ClassName;` (semicolon, not braces). Even classes with no ivars need `#[ivars = ()]`.
- `self.ivars()` requires `use objc2::DefinedClass` in scope.
- `NSStatusBar::systemStatusBar()` takes NO arguments (not mtm).
- `NSMenuItem::setTarget` IS unsafe.
- `DashboardHelper::alloc()` needs `use objc2::AnyThread`.

## rmcp API Notes (v1.1)

- `InitializeResult.server_info` is `Implementation` directly (NOT `Option<Implementation>`)
- `Implementation` has: `name`, `version`, `title: Option`, `description: Option`, `icons: Option`, `website_url: Option`
- `Tool` has: `name`, `title: Option`, `description: Option`, `input_schema`, `annotations: Option<ToolAnnotations>`
- `ToolAnnotations` fields: `read_only_hint`, `destructive_hint`, `idempotent_hint`, `open_world_hint` (all `Option<bool>`)
- `ServerCapabilities` fields: `tools`, `resources`, `prompts`, `logging` (all `Option<...>`)
- Most model types are `#[non_exhaustive]` — use constructors, not struct literals
- `ListToolsResult { tools, next_cursor: None, meta: None }` — has `meta` field, NOT non_exhaustive
- `rmcp::ErrorData` (not `rmcp::Error`) — `ErrorData::invalid_params(msg, data)`
- Cancel via `client.cancellation_token().cancel()` (not `client.cancel()` which consumes self)
