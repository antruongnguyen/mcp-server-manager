# MCPSM — MCP Server Manager

A native macOS application that manages [Model Context Protocol](https://modelcontextprotocol.io/) (MCP) server instances and acts as a **unified MCP proxy**. Configure your MCP servers once, then point any client (Claude Code, Cline, GitHub Copilot, Cursor, etc.) at a single endpoint — MCPSM aggregates tools from all running servers automatically.

## Features

- **Unified MCP proxy** — single endpoint aggregates tools from all child servers
- **Streamable HTTP transport** — full MCP protocol support (POST/GET/DELETE) with session management via [rmcp](https://github.com/anthropics/rmcp) SDK
- **Tool namespacing** — tools from each server are prefixed (`server_id__tool_name`) to avoid collisions
- **Web dashboard** — start/stop servers, auto-start toggle, view rich tool info, monitor logs, dark/light theme
- **Rich tool info** — tool descriptions, annotations (read-only, destructive, idempotent), MCP server info panel with capabilities
- **Dark/light theme** — Islands-inspired color scheme with theme toggle in header (persisted in localStorage)
- **Server auto-start** — non-disabled servers start automatically on launch
- **Auto-start toggle** — per-server toggle in dashboard; toggling only saves config (does not start/stop running servers)
- **Start/Stop All** — header buttons for bulk server control (conditional visibility based on server states)
- **Configurable port** — set `"port": 8080` in config to change the dashboard/proxy port (default: 3456)
- **Config file watching** — external edits to the config file are detected and applied automatically (smart diff: only affected servers restart)
- **Status bar app** — macOS status bar with template icon, SF Symbol menu icons, "Open Dashboard", "Edit Config", and "Quit"
- **Shell environment capture** — captures user's full login shell environment at startup, so child processes find tools installed via nvm, pyenv, Homebrew, etc. (critical for `.app` bundles)
- **`.app` bundle** — builds as a proper macOS application bundle (`MCPSM.app`) with app icon and `LSUIElement` (no Dock icon)
- **Edit Config** — opens `~/.config/mcpsm/mcp.json` in your default editor from the status bar
- **Real-time log viewer** — monospaced log panel with auto-scroll (last 200 lines per server)
- **Built-in templates** — pre-configured servers for Sequential Thinking, Knowledge Graph Memory, and Context7
- **Auto-save** — server config is persisted to disk immediately on add/update/delete/disabled toggle
- **Graceful shutdown** — Quit and Ctrl+C stop all running servers cleanly (SIGTERM → 5s → SIGKILL)

## Requirements

- Rust (edition 2024)
- macOS (for the GUI binary — the CLI binary is cross-platform)
- Node.js / `npx` (for running MCP servers that use it)

## Building

```bash
cargo build --workspace          # Build all crates (debug)
cargo build --release -p mcpsm-cli   # Release build — CLI only
cargo build --release -p mcpsm-gui   # Release build — GUI only
```

### Building as .app Bundle (macOS)

```bash
./scripts/build-app.sh
open target/release/MCPSM.app
```

This creates a proper macOS application bundle with app icon and `LSUIElement` (no Dock icon). The `.app` bundle captures your shell environment at launch, so tools installed via nvm, pyenv, Homebrew, etc. work correctly.

## Running

**Headless CLI** (no GUI, runs on any platform):

```bash
cargo run -p mcpsm-cli
```

**macOS GUI** (status bar app):

```bash
cargo run -p mcpsm-gui
```

On launch, MCPSM reads server configuration from `~/.config/mcpsm/mcp.json` (auto-created on first run). Non-disabled servers start automatically. Servers added through the web dashboard are saved to this file automatically.

The config format is:

```json
{
  "port": 3456,
  "mcpServers": {
    "sequential-thinking": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-sequential-thinking"],
      "env": {}
    },
    "memory": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-memory"],
      "env": {
        "MEMORY_FILE_PATH": "/tmp/memory.json"
      }
    },
    "remote-server": {
      "url": "http://example.com:3000/mcp",
      "disabled": true
    }
  }
}
```

**Config fields:**
- `port` (optional) — web dashboard and proxy port (default: `3456`)
- `mcpServers` — map of server ID → config
  - `command` + `args` + `env` — for stdio (child process) servers
  - `url` — for remote HTTP servers
  - `disabled` (optional, default: `false`) — when `true`, the server won't auto-start on launch (omitted from JSON when `false`)

**Config file watching:** MCPSM watches the config file for external changes. When you edit it in an editor, MCPSM detects the change and applies a smart diff — only affected servers are stopped/started/restarted.

## Connecting MCP Clients

Once MCPSM is running and servers show **Ready** status in the dashboard, point your MCP client at the proxy endpoint:

**Streamable HTTP**: `http://127.0.0.1:3456/mcp` (or your configured port)

### Claude Code

```bash
claude mcp add mcpsm --transport http http://127.0.0.1:3456/mcp
```

### GitHub Copilot (VS Code)

Create or edit `.vscode/mcp.json` in your workspace (or open **MCP: Open User Configuration** from the command palette for global config):

```json
{
  "servers": {
    "mcpsm": {
      "type": "http",
      "url": "http://127.0.0.1:3456/mcp"
    }
  }
}
```

### Cline (VS Code)

```json
{
  "mcpServers": {
    "mcpsm": {
      "url": "http://127.0.0.1:3456/mcp",
      "disabled": false
    }
  }
}
```

### Cursor

Add to your Cursor MCP settings (`~/.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "mcpsm": {
      "url": "http://127.0.0.1:3456/mcp"
    }
  }
}
```

### Any MCP Client

Point to `http://127.0.0.1:3456/mcp` — any client supporting [Streamable HTTP transport](https://modelcontextprotocol.io/docs/concepts/transports#streamable-http) will work. The endpoint handles `POST` (JSON-RPC requests), `GET` (SSE notifications), and `DELETE` (session teardown).

## Server Lifecycle

```
Stopped → Starting → Initializing → Ready (with tools)
                                      ↕
                     Stopping ← ──────┘
```

When a server starts, MCPSM:
1. Spawns the process with piped stdin/stdout/stderr (or connects to remote URL)
2. Performs MCP `initialize` handshake via rmcp SDK
3. Calls `tools/list` to discover available tools (with descriptions and annotations)
4. Transitions to **Ready** — tools become available through the proxy
5. Displays MCP server info: name, version, protocol version, capabilities

## Architecture

MCPSM is organized as a Cargo workspace with three crates:

- **`mcpsm-core`** — shared library with all business logic (config, server manager, MCP proxy, web dashboard)
- **`mcpsm-cli`** — headless CLI daemon (`mcpsm` binary) — runs the backend on the main thread's tokio runtime
- **`mcpsm-gui`** — macOS status bar app (`mcpsm-gui` binary) — two-thread design: AppKit on main, tokio on background

```
External MCP Client (Claude Code, Cline, etc.)
        │
        │ Streamable HTTP: POST/GET/DELETE /mcp
        ▼
┌─ axum :{port} ────────────────────────────────┐
│  GET /          → web dashboard               │
│  /api/*         → REST management API         │
│  POST /mcp      → JSON-RPC request handling   │
│  GET  /mcp      → SSE for server notifications│
│  DELETE /mcp    → session teardown            │
└─────────┬─────────────────────────────────────┘
          │
┌─────────▼─────────────────────────────────────┐
│  ProxyHandler (proxy.rs)                      │
│  - tools/list → aggregated + namespaced tools │
│  - tools/call → route to correct child        │
└─────────┬─────────────────────────────────────┘
          │ (strip namespace prefix, forward)
┌─────────▼─────────────────────────────────────┐
│  McpClient (rmcp SDK) per server              │
│  - stdio: JSON-RPC over stdin/stdout          │
│  - http: StreamableHttpClientTransport        │
│  - initialize → tools/list → Ready            │
└─────────┬──────────────┬──────────────────────┘
     stdin│        stdout│ (JSON-RPC)   stderr│ (logs)
          ▼              ▼                    ▼
     Child MCP Server Process             LogBuffer
```

## Project Structure

```
Cargo.toml                          # Workspace root
crates/
├── mcpsm-core/                     # Shared library
│   └── src/
│       ├── lib.rs                  # Public module exports
│       ├── runtime.rs              # run_backend() — shared async entry point
│       ├── core/
│       │   ├── config.rs           # Config I/O (~/.config/mcpsm/mcp.json), port extraction
│       │   ├── server.rs           # ServerConfig, ServerStatus, ToolInfo, McpPeerInfo types
│       │   ├── process.rs          # Child process management (SIGTERM/SIGKILL, stderr reader)
│       │   ├── log_buffer.rs       # Ring buffer (200 lines per server)
│       │   ├── shell_env.rs        # Shell environment capture ($SHELL -l -c env)
│       │   ├── templates.rs        # Built-in server templates
│       │   ├── manager.rs          # Async orchestrator: auto-start, config reload, enable/disable
│       │   └── watcher.rs          # Config file watcher (notify crate)
│       ├── bridge/
│       │   └── commands.rs         # AppCommand + BackendEvent enums
│       ├── mcp/
│       │   ├── client.rs           # rmcp SDK McpClient wrapper (stdio + HTTP connect)
│       │   └── proxy.rs            # Tool aggregation + request routing (ProxyHandler)
│       └── web/
│           ├── state.rs            # Shared AppState
│           ├── server.rs           # Router + web server startup (configurable port)
│           ├── handlers.rs         # REST API handlers (CRUD + disabled toggle + start/stop all)
│           ├── sse.rs              # Server-Sent Events stream
│           └── dashboard.html      # Web dashboard UI
├── mcpsm-cli/                      # Headless CLI daemon
│   └── src/main.rs                 # Tokio runtime on main thread, Ctrl+C shutdown
└── mcpsm-gui/                      # macOS status bar app
    └── src/
        ├── main.rs                 # Two-thread: AppKit main + tokio background
        └── gui/
            └── status_bar.rs       # Status bar (template icon, SF Symbols, menus)
resources/                          # Status bar PNGs, .icns
scripts/                            # build-app.sh, generate-icons.sh
```

## Management REST API

| Method | Path                       | Description                              |
|--------|----------------------------|------------------------------------------|
| GET    | `/`                        | Web dashboard                            |
| GET    | `/api/servers`             | List servers with status, tools, peer info |
| POST   | `/api/servers`             | Add a server (auto-saves to config)      |
| POST   | `/api/servers/:id/start`   | Start a server                           |
| POST   | `/api/servers/:id/stop`    | Stop a server                            |
| POST   | `/api/servers/:id/disabled` | Set disabled flag (`{ "disabled": bool }`)   |
| PUT    | `/api/servers/:id`         | Update server config                         |
| DELETE | `/api/servers/:id`         | Remove a server (auto-saves to config)       |
| GET    | `/api/servers/:id/logs`    | Request log snapshot                         |
| DELETE | `/api/servers/:id/logs`    | Clear server logs                            |
| POST   | `/api/servers/start-all`   | Start all non-disabled servers               |
| POST   | `/api/servers/stop-all`    | Stop all running servers                     |
| GET    | `/api/templates`           | List built-in templates                      |
| GET    | `/api/events`              | SSE event stream                         |

## MCP Proxy Endpoint

| Method | Path   | Headers                       | Description                                                          |
|--------|--------|-------------------------------|----------------------------------------------------------------------|
| POST   | `/mcp` | `Mcp-Session-Id` (after init) | JSON-RPC requests (`initialize`, `tools/list`, `tools/call`, `ping`) |
| GET    | `/mcp` | `Mcp-Session-Id`              | SSE stream for server-to-client notifications                        |
| DELETE | `/mcp` | `Mcp-Session-Id`              | Tear down session                                                    |

## License

MIT
