# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
cargo build --workspace      # Debug build (all crates)
cargo build --release -p mcpsm-cli   # Release CLI binary
cargo build --release -p mcpsm-gui   # Release GUI binary
cargo run -p mcpsm-cli       # Run headless CLI daemon
cargo run -p mcpsm-gui       # Run macOS GUI app
cargo test --workspace       # Run all 9 unit tests
cargo test -p mcpsm-core config      # Run config tests only
cargo test -p mcpsm-core log_buffer  # Run log buffer tests only
cargo test -p mcpsm-core templates   # Run template tests only
./scripts/build-app.sh       # Build MCPSM.app bundle (release, uses mcpsm-gui)
./scripts/generate-icons.sh  # Regenerate status bar PNGs + .icns from SVGs
```

## Architecture

Cargo workspace with three crates:

- **`mcpsm-core`** (library) — all business logic, shared by both binaries
- **`mcpsm-cli`** (binary: `mcpsm`) — headless CLI daemon, tokio runtime on main thread
- **`mcpsm-gui`** (binary: `mcpsm-gui`) — macOS status bar app, two-thread design

Two-thread design (GUI only) with a channel bridge:

- **Main thread**: AppKit status bar (NSStatusItem via objc2)
- **Background thread**: tokio async runtime via `mcpsm_core::runtime::run_backend()`

Communication:
- Web/GUI → Backend: `tokio::sync::mpsc::unbounded_channel<AppCommand>`
- Backend → Web: `tokio::sync::broadcast::channel<BackendEvent>` (SSE streaming)
- Shared state: `Arc<RwLock<HashMap<String, ServerInfo>>>` for web layer reads
- Shared MCP clients: `Arc<RwLock<HashMap<String, Arc<McpClient>>>>` for proxy access
- Tool changes: `tokio::sync::watch::channel<()>` signals proxy when tool lists change
- Config watcher: `notify` crate on a dedicated std::thread, sends `ReloadConfigIfChanged` through cmd channel

The bridge contract lives in `crates/mcpsm-core/src/bridge/commands.rs` — all interaction goes through `AppCommand` and `BackendEvent` enums.

## Module Layout

**`crates/mcpsm-core/src/`** — shared library:
- **`runtime.rs`** — `run_backend()` async entry point (creates channels, proxy, manager, web server)
- **`core/`** — Business logic: config I/O, process spawn/kill, log ring buffer, server manager async loop, config file watcher, shell environment capture
- **`bridge/`** — Channel contract: `AppCommand` + `BackendEvent` enums
- **`mcp/`** — MCP protocol via rmcp SDK: client wrapper (stdio + HTTP), proxy handler (tool aggregation + routing)
- **`web/`** — axum web server: REST API, SSE, dashboard HTML, MCP endpoint

**`crates/mcpsm-cli/src/`** — headless CLI binary (depends on mcpsm-core only)

**`crates/mcpsm-gui/src/`** — macOS GUI binary (depends on mcpsm-core + objc2):
- **`gui/`** — macOS status bar (template icon, SF Symbol menu icons, "Open Dashboard" + "Edit Config" + "Quit" with graceful shutdown)

## Dashboard Theme System

The dashboard uses CSS custom properties with `[data-theme="dark"]` / `[data-theme="light"]` selectors, inspired by the Islands VS Code theme. Theme is applied before first paint via inline `<script>` in `<head>` and persisted in `localStorage` as `mcpsm-theme`.

Key color variable groups:
- `--bg`, `--surface`, `--surface2`, `--border` — layer hierarchy
- `--text`, `--text2` — primary/secondary text
- `--accent`, `--green`, `--yellow`, `--red`, `--blue` — semantic colors
- `--green-bg`, `--yellow-bg`, `--red-bg`, `--blue-bg` — tinted badge backgrounds
- `--logo-*` — separate logo-specific colors tuned per theme for header visibility

## MCP Proxy

MCPSM acts as a unified MCP proxy server at `http://127.0.0.1:{port}/mcp` (port configurable, default 3456):

- **Child communication**: rmcp SDK over stdin/stdout (stdio) or StreamableHttpClientTransport (HTTP)
- **McpClient** (`mcp/client.rs`): type alias for `RunningService<RoleClient, ClientInfo>`, connect_stdio/connect_http wrappers
- **ProxyHandler** (`mcp/proxy.rs`): implements `rmcp::handler::server::ServerHandler`, tool aggregation with namespacing (`server_id__tool_name`)
- **Transport**: rmcp's `StreamableHttpService` mounted via `.nest_service("/mcp", ...)`
- **Server lifecycle**: Stopped → Starting → Initializing (MCP handshake) → Ready (tools discovered)

## Config File

Reads/writes `~/.config/mcpsm/mcp.json` (auto-created on first run). Uses `serde_json::Value` for the outer document to **preserve unknown fields** — only the `mcpServers` and `port` keys are modified. Writes are atomic (temp file + rename).

Config supports:
- `port` (u16, default 3456) — extracted at startup, persisted if non-default
- `mcpServers` — map of server configs with `command`/`args`/`env` (stdio) or `url` (HTTP) + optional `disabled` (default false, omitted when false)

Config file watcher (`core/watcher.rs`) uses `notify` crate with 500ms debounce. Smart reload diffs old vs new config: stops removed servers, adds new ones (auto-starts if not disabled), restarts changed ones. Ignores events within 2s of our own saves.

## Server Disabled Flag

`ServerConfig.disabled` field (`#[serde(default, skip_serializing_if = "std::ops::Not::not")]`). Non-disabled servers auto-start on launch. The `SetServerDisabled` command updates config and saves; disabling a running server stops it, but enabling does NOT auto-start — the user must click Start. Dashboard shows an enable/disable toggle switch. Start All / Stop All buttons in header for bulk operations.

## Rich Tool/Server Info

- `ToolInfo`: name, title, description, annotations (read_only_hint, destructive_hint, idempotent_hint, open_world_hint)
- `McpPeerInfo`: name, version, title, description, protocol_version, instructions, capabilities
- Converted from rmcp types (`Tool` → `ToolInfo`, `ServerInfo/Implementation` → `McpPeerInfo`) in manager.rs
- Dashboard renders tool descriptions, annotation badges, and MCP server info panel with capability badges

## Process Management

Servers are spawned via `tokio::process::Command` with piped stdin/stdout/stderr:
- **stdin/stdout** — reserved for MCP JSON-RPC protocol communication (rmcp SDK)
- **stderr** — captured as log lines by a tokio task, fed into the manager's `select!` loop

Shell environment is captured once at startup via `core::shell_env::capture_shell_env()` (runs `$SHELL -l -c env`), then passed to all child processes via `env_clear()` + `envs()`. This ensures tools installed via nvm, pyenv, Homebrew, etc. are found even when running as a `.app` bundle. Config env vars override the captured environment. Graceful shutdown: SIGTERM → 5s wait → SIGKILL.

## Status Bar

- Template icon (18×18 / 36×36 PNG) embedded via `include_bytes!`, loaded as NSImage with `setTemplate(true)` for automatic light/dark mode
- SF Symbol icons on menu items: "Open Dashboard" (gauge), "Edit Config" (doc.badge.gearshape), "Quit MCPSM" (power)
- Disabled "MCP Server Manager" title item at top of menu
- `static CMD_TX: OnceLock<UnboundedSender<AppCommand>>` — set from main.rs, used by Quit to send Shutdown
- `static DASHBOARD_PORT: OnceLock<u16>` — set from main.rs, used for dynamic dashboard URL
- Ctrl+C signal handler also sends `AppCommand::Shutdown`

## .app Bundle

`Info.plist` at project root with `LSUIElement: true` (no Dock icon). Built via `scripts/build-app.sh`:
- Builds the `mcpsm-gui` crate in release mode
- `MCPSM.app/Contents/MacOS/mcpsm` — release binary (from `target/release/mcpsm-gui`)
- `MCPSM.app/Contents/Resources/mcpsm.icns` — app icon
- `MCPSM.app/Contents/Info.plist` — bundle metadata

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
