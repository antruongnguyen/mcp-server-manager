# Project Ideas

- Project name: MCP Server Manager
- Project description: A native macOS GUI application to manage MCP server instances.
- Target audience: Developers and users who want to easily manage their MCP servers on macOS.

## Technologies

- Rust programming language
- Framework and library:
  - tokio (async runtime)
  - serde, serde_json (config serialization)
  - tracing, tracing-subscriber (structured logging)
  - dirs (platform directory paths)
  - GUI: objc2, objc2-foundation, objc2-app-kit
  - Future: Rig (rig.rs) for AI-assisted features

## Features

- Native macOS GUI for starting, stopping, and managing MCP server instances and tools
- Real-time server status monitoring and logs: last 200 lines of logs
- Customizable server settings and configurations (command, args, env per MCP server)
- macOS only for v0.1
- Modular design to support additional features and integrations
- Add support for more server types and tools
- Pre-configured some MCP servers and tools:
  - Sequential Thinking: https://github.com/modelcontextprotocol/servers/tree/main/src/sequentialthinking
  - Knowledge Graph Memory Server: https://github.com/modelcontextprotocol/servers/tree/main/src/memory
  - Context7: https://github.com/upstash/context7
