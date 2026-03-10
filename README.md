# MCPSM — MCP Server Manager

A native macOS application that manages [Model Context Protocol](https://modelcontextprotocol.io/) (MCP) server instances and acts as a **unified MCP proxy**. Configure your MCP servers once, then point any client (Claude Code, Cline, GitHub Copilot, Cursor, etc.) at a single endpoint — MCPSM aggregates tools from all running servers automatically.

## Features

- **Unified MCP proxy** — single endpoint aggregates tools from all child servers
- **Dual transport support** — Streamable HTTP (`/mcp`) for Claude Code, Copilot, Cursor + legacy SSE (`/sse`) for Cline and older clients
- **Streamable HTTP transport** (2025-03-26) — full MCP protocol support (POST/GET/DELETE) with session management
- **Legacy SSE transport** (2024-11-05) — `GET /sse` + `POST /message` for clients that don't support HTTP transport
- **Tool namespacing** — tools from each server are prefixed (`server_id__tool_name`) to avoid collisions
- **Web dashboard** — start/stop servers, view tools, monitor logs at `http://127.0.0.1:17532`
- **Status bar app** — minimal macOS status bar icon with "Open Dashboard" menu
- **Real-time log viewer** — monospaced log panel with auto-scroll (last 200 lines per server)
- **Built-in templates** — pre-configured servers for Sequential Thinking, Knowledge Graph Memory, and Context7
- **Auto-save** — server config is persisted to disk immediately on add/update/delete
- **Graceful shutdown** — SIGTERM with timeout, then SIGKILL; all child processes cleaned up on quit

## Requirements

- macOS (native AppKit status bar via objc2)
- Rust (edition 2024)
- Node.js / `npx` (for running MCP servers that use it)

## Building

```bash
cargo build --release
```

## Running

```bash
cargo run
```

On launch, MCPSM reads server configuration from `~/.config/mcpsm/mcp.json` (auto-created on first run). Servers added through the web dashboard are saved to this file automatically.

The config format is:

```json
{
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
    }
  }
}
```

## Connecting MCP Clients

Once MCPSM is running and servers show **Ready** status in the dashboard, point your MCP client at one of the proxy endpoints:

- **Streamable HTTP** (recommended): `http://127.0.0.1:17532/mcp` — for Claude Code, GitHub Copilot, Cursor
- **Legacy SSE**: `http://127.0.0.1:17532/sse` — for Cline and clients that only support SSE transport

### Claude Code

```bash
claude mcp add mcpsm --transport http http://127.0.0.1:17532/mcp
```

This adds MCPSM to your Claude Code MCP configuration. All tools from your running servers will appear in Claude Code automatically.

### GitHub Copilot (VS Code)

Create or edit `.vscode/mcp.json` in your workspace (or open **MCP: Open User Configuration** from the command palette for global config):

```json
{
  "servers": {
    "mcpsm": {
      "type": "http",
      "url": "http://127.0.0.1:17532/mcp"
    }
  }
}
```

Copilot will discover all aggregated tools from MCPSM when using Agent mode.

### Cline (VS Code)

Cline uses the **legacy SSE transport**. Open Cline's MCP settings (gear icon in the Cline sidebar, or edit `~/Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json` directly):

```json
{
  "mcpServers": {
    "mcpsm": {
      "url": "http://127.0.0.1:17532/sse",
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
      "url": "http://127.0.0.1:17532/mcp"
    }
  }
}
```

### Any MCP Client

MCPSM supports two transports:

- **Streamable HTTP** (2025-03-26): Point to `http://127.0.0.1:17532/mcp` — used by Claude Code, GitHub Copilot, Cursor, and any client supporting the [Streamable HTTP transport](https://modelcontextprotocol.io/docs/concepts/transports#streamable-http).
- **Legacy SSE** (2024-11-05): Point to `http://127.0.0.1:17532/sse` — used by Cline and clients that implement the older [SSE transport](https://modelcontextprotocol.io/docs/concepts/transports#http-with-sse). The client opens an SSE connection to `/sse`, receives a `POST` endpoint URL, then sends JSON-RPC messages to `/message?sessionId=xxx`.

## How It Works: MCP Protocol Flow

MCPSM speaks the [Model Context Protocol](https://modelcontextprotocol.io/) over two transports. The protocol messages are the same — only the transport layer differs.

### Streamable HTTP Transport (2025-03-26)

Used by Claude Code, GitHub Copilot, Cursor. All communication happens over `POST /mcp`.

#### 1. Initialize — establish a session

The client sends an `initialize` request:

```bash
curl -X POST http://127.0.0.1:17532/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2025-03-26",
      "capabilities": {},
      "clientInfo": { "name": "my-client", "version": "1.0" }
    }
  }'
```

MCPSM responds with its capabilities and a **session ID** in the `Mcp-Session-Id` response header:

```
HTTP/1.1 200 OK
Content-Type: application/json
Mcp-Session-Id: a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2025-03-26",
    "capabilities": { "tools": { "listChanged": true } },
    "serverInfo": { "name": "mcpsm", "version": "0.1.0" }
  }
}
```

The `Mcp-Session-Id` is a UUID that identifies this session. **All subsequent requests must include this header.** This is how MCPSM tracks which client is which. Clients do not need to generate this ID — it is created by MCPSM and returned during initialization.

The client then confirms initialization:

```bash
curl -X POST http://127.0.0.1:17532/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: a1b2c3d4-e5f6-7890-abcd-ef1234567890" \
  -d '{"jsonrpc": "2.0", "method": "notifications/initialized"}'
```

#### 2. List tools — discover what's available

The client calls `tools/list` to get all available tools:

```bash
curl -X POST http://127.0.0.1:17532/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: a1b2c3d4-e5f6-7890-abcd-ef1234567890" \
  -d '{"jsonrpc": "2.0", "id": 2, "method": "tools/list"}'
```

MCPSM responds with tools **aggregated from all running child servers**, namespaced with the server ID:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "tools": [
      {
        "name": "sequential-thinking__sequentialthinking",
        "description": "[sequential-thinking] A tool for dynamic problem-solving through thoughts...",
        "inputSchema": {
          "type": "object",
          "properties": {
            "thought": { "type": "string", "description": "Your current thinking step" },
            "nextThoughtNeeded": { "type": "boolean" },
            "thoughtNumber": { "type": "integer", "minimum": 1 },
            "totalThoughts": { "type": "integer", "minimum": 1 }
          },
          "required": ["thought", "nextThoughtNeeded", "thoughtNumber", "totalThoughts"]
        }
      },
      {
        "name": "memory__create_entities",
        "description": "[memory] Create multiple new entities in the knowledge graph",
        "inputSchema": {
          "type": "object",
          "properties": {
            "entities": { "type": "array", "items": { "type": "object" } }
          },
          "required": ["entities"]
        }
      }
    ]
  }
}
```

Each tool includes:
- **`name`** — namespaced as `server_id__tool_name` (double underscore separator)
- **`description`** — prefixed with `[server_id]` for clarity, followed by the tool's original description
- **`inputSchema`** — JSON Schema describing the tool's parameters (types, required fields, descriptions)

The AI agent uses `inputSchema` to understand what arguments each tool accepts and how to call it correctly.

#### 3. Call a tool — execute it on the right server

The client calls a tool using its namespaced name:

```bash
curl -X POST http://127.0.0.1:17532/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: a1b2c3d4-e5f6-7890-abcd-ef1234567890" \
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "tools/call",
    "params": {
      "name": "sequential-thinking__sequentialthinking",
      "arguments": {
        "thought": "Let me analyze this problem step by step",
        "nextThoughtNeeded": true,
        "thoughtNumber": 1,
        "totalThoughts": 5
      }
    }
  }'
```

MCPSM strips the namespace prefix (`sequential-thinking__` -> `sequentialthinking`), routes the call to the correct child server, and returns the result:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [
      { "type": "text", "text": "Thought recorded. Continue with your analysis." }
    ]
  }
}
```

#### 4. Tool change notifications — via SSE

If a server's tools change (e.g., a new server starts or stops), MCPSM sends a `notifications/tools/list_changed` notification over the SSE stream. Clients that opened a `GET /mcp` SSE connection receive this and should re-fetch `tools/list`.

#### 5. End session

When the client is done:

```bash
curl -X DELETE http://127.0.0.1:17532/mcp \
  -H "Mcp-Session-Id: a1b2c3d4-e5f6-7890-abcd-ef1234567890"
```

#### Streamable HTTP Summary

| Step | Method | What happens |
|------|--------|-------------|
| Initialize | `POST /mcp` with `initialize` | Creates session, returns `Mcp-Session-Id` header |
| Confirm | `POST /mcp` with `notifications/initialized` | Marks session as ready |
| List tools | `POST /mcp` with `tools/list` | Returns all tools with names, descriptions, and input schemas |
| Call tool | `POST /mcp` with `tools/call` | Routes to correct child server, returns result |
| Listen | `GET /mcp` | SSE stream for tool change notifications |
| Disconnect | `DELETE /mcp` | Tears down session |

In practice, MCP clients handle all of this automatically — you just need to configure the endpoint URL.

### Legacy SSE Transport (2024-11-05)

Used by Cline and older MCP clients. The client opens an SSE stream, then sends JSON-RPC messages to a separate POST endpoint. Responses are delivered through the SSE stream, not in the HTTP response body.

#### 1. Connect — open SSE stream

The client opens an SSE connection:

```bash
curl -N http://127.0.0.1:17532/sse
```

MCPSM sends an `endpoint` event with the POST URL for this session:

```
event: endpoint
data: /message?sessionId=a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

#### 2. Initialize — via POST to message endpoint

The client sends `initialize` to the endpoint URL received above:

```bash
curl -X POST "http://127.0.0.1:17532/message?sessionId=a1b2c3d4-e5f6-7890-abcd-ef1234567890" \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2025-03-26",
      "capabilities": {},
      "clientInfo": { "name": "my-client", "version": "1.0" }
    }
  }'
```

The HTTP response is `202 Accepted` (empty). The actual JSON-RPC response arrives on the SSE stream:

```
event: message
data: {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"mcpsm","version":"0.1.0"}}}
```

#### 3. List tools and call tools

All subsequent requests (`tools/list`, `tools/call`, `ping`, etc.) follow the same pattern: POST to `/message?sessionId=xxx`, get `202 Accepted`, receive the response on the SSE stream.

#### 4. Tool change notifications

When tools change (server starts/stops), MCPSM pushes a notification through the SSE stream:

```
event: message
data: {"jsonrpc":"2.0","method":"notifications/tools/list_changed"}
```

The client should then re-fetch `tools/list`.

#### SSE Transport Summary

| Step | Action | What happens |
|------|--------|-------------|
| Connect | `GET /sse` | Opens SSE stream, receives `endpoint` event with POST URL |
| Send messages | `POST /message?sessionId=xxx` | All JSON-RPC requests; returns 202, response on SSE stream |
| Receive | SSE `message` events | JSON-RPC responses + notifications delivered via SSE |

## Server Lifecycle

```
Stopped → Starting → Initializing → Ready (with tools)
                                      ↕
                     Stopping ← ──────┘
```

When a server starts, MCPSM:
1. Spawns the process with piped stdin/stdout/stderr
2. Performs MCP `initialize` handshake over stdin/stdout (JSON-RPC 2.0)
3. Calls `tools/list` to discover available tools
4. Transitions to **Ready** — tools become available through the proxy
5. Listens for `notifications/tools/list_changed` from the child to re-fetch tools dynamically

## Architecture

```
External MCP Client (Claude Code, Cline, etc.)
        │
        │ Streamable HTTP: POST/GET/DELETE /mcp
        │ Legacy SSE: GET /sse + POST /message
        ▼
┌─ axum :17532 ───────────────────────────────────┐
│  GET /          → web dashboard                 │
│  /api/*         → REST management API           │
│  POST /mcp      → JSON-RPC request handling     │
│  GET  /mcp      → SSE for server notifications  │
│  DELETE /mcp    → session teardown              │
│  GET  /sse      → legacy SSE transport connect  │
│  POST /message  → legacy SSE message endpoint   │
└─────────┬───────────────────────────────────────┘
          │
┌─────────▼───────────────────────────────────────┐
│  McpProxy (proxy.rs)                            │
│  - Session management (Mcp-Session-Id)          │
│  - tools/list → aggregated + namespaced tools   │
│  - tools/call → route to correct child          │
└─────────┬───────────────────────────────────────┘
          │ (strip namespace prefix, forward)
┌─────────▼───────────────────────────────────────┐
│  McpChildClient (child_client.rs) per server    │
│  - JSON-RPC 2.0 over stdin/stdout               │
│  - Request ID tracking + oneshot responses      │
│  - initialize → tools/list → Ready              │
└─────────┬──────────────┬────────────────────────┘
     stdin│        stdout│ (JSON-RPC)    stderr│ (logs)
          ▼              ▼                     ▼
     Child MCP Server Process              LogBuffer
```

## Project Structure

```
src/
├── main.rs                     # Entry point: spawn tokio thread, start status bar
├── core/
│   ├── config.rs               # Config I/O (~/.config/mcpsm/mcp.json)
│   ├── server.rs               # ServerConfig, ServerStatus types
│   ├── process.rs              # Child process spawn with piped stdin/stdout/stderr
│   ├── log_buffer.rs           # Ring buffer (200 lines per server)
│   ├── templates.rs            # Built-in server templates
│   └── manager.rs              # Async orchestrator with MCP client lifecycle
├── bridge/
│   └── commands.rs             # AppCommand + BackendEvent enums
├── gui/
│   └── status_bar.rs           # macOS status bar (NSStatusItem)
├── mcp/
│   ├── jsonrpc.rs              # JSON-RPC 2.0 message types
│   ├── types.rs                # MCP protocol types (Tool, Content, etc.)
│   ├── child_client.rs         # Per-child stdio JSON-RPC client
│   ├── proxy.rs                # Tool aggregation, session mgmt, request routing
│   └── transport.rs            # Streamable HTTP + legacy SSE axum handlers
└── web/
    ├── state.rs                # Shared AppState
    ├── server.rs               # Router + web server startup
    ├── handlers.rs             # REST API handlers
    ├── sse.rs                  # Server-Sent Events stream
    └── dashboard.html          # Web dashboard UI
```

## Management REST API

| Method | Path                     | Description                            |
|--------|--------------------------|----------------------------------------|
| GET    | `/`                      | Web dashboard                          |
| GET    | `/api/servers`           | List servers with status + tools       |
| POST   | `/api/servers`           | Add a server (auto-saves to config)    |
| POST   | `/api/servers/:id/start` | Start a server                         |
| POST   | `/api/servers/:id/stop`  | Stop a server                          |
| DELETE | `/api/servers/:id`       | Remove a server (auto-saves to config) |
| GET    | `/api/servers/:id/logs`  | Request log snapshot                   |
| GET    | `/api/templates`         | List built-in templates                |
| GET    | `/api/events`            | SSE event stream                       |

## MCP Proxy Endpoints

### Streamable HTTP (2025-03-26)

| Method | Path   | Headers                       | Description                                                          |
|--------|--------|-------------------------------|----------------------------------------------------------------------|
| POST   | `/mcp` | `Mcp-Session-Id` (after init) | JSON-RPC requests (`initialize`, `tools/list`, `tools/call`, `ping`) |
| GET    | `/mcp` | `Mcp-Session-Id`              | SSE stream for server-to-client notifications                        |
| DELETE | `/mcp` | `Mcp-Session-Id`              | Tear down session                                                    |

### Legacy SSE (2024-11-05)

| Method | Path                     | Description                                                  |
|--------|--------------------------|--------------------------------------------------------------|
| GET    | `/sse`                   | Open SSE connection; receives `endpoint` event with POST URL |
| POST   | `/message?sessionId=xxx` | Send JSON-RPC messages; responses arrive on SSE stream       |

## License

MIT
