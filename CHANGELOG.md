# Changelog

All notable changes to MCPSM (MCP Server Manager) are documented in this file.

## [1.1.1] - 2026-03-31

### Added

- **HTTP headers support** for remote MCP servers — replaces single `auth_header` with general `headers` map for flexible authentication and custom headers
- **Dashboard home panel improvements**: Resources and Prompts stat cards, registry links, GitHub repo link, server IDs, and additional CSS classes
- Reusable StatCard and RegistryItem dashboard components

### Fixed

- MCP server info separator hidden when no peer info is available

## [1.1.0] - 2026-03-31

### Added

- **MCP Resources pass-through**: list, read, and template operations proxied to child servers
- **MCP Prompts pass-through**: list and get operations proxied to child servers
- **MCP Logging pass-through**: log level control forwarded to child servers
- Resources tab with search integration in the dashboard
- Prompts tab with search integration in the dashboard
- Logging tab with level selector and structured MCP log message viewer

### Fixed

- `applyTemplate` now correctly handles HTTP transport templates

### Changed

- Updated GitHub Actions workflow dependencies

## [1.0.0] - 2026-03-30

### Added

- **Unified MCP proxy** at `/mcp` endpoint — external clients connect once, tools from all child servers are aggregated with `server_id__tool_name` namespacing
- **MCP client** supporting both stdio and HTTP (Streamable HTTP) transports via rmcp SDK v1.1
- **Web dashboard** with dark/light theme toggle (Islands-inspired), logo, favicon, and CSS custom properties
- **Server management**: add/remove/start/stop servers, bulk start/stop, auto-start on launch with parallel initialization
- **macOS status bar app** (`mcpsm-gui`) with template icon, SF Symbol menu icons, and "Open Dashboard" / "Edit Config" / "Quit" actions
- **Headless CLI daemon** (`mcpsm-cli`) with Ctrl+C graceful shutdown
- **Config file** at `~/.config/mcpsm/mcp.json` with atomic writes, auto-creation, and unknown field preservation
- **Config file watcher** with 500ms debounce — smart reload diffs old vs new, starts/stops/restarts servers as needed
- **Shell environment capture** (`$SHELL -l -c env`) so child processes find tools installed via nvm, pyenv, Homebrew, etc.
- **Cross-platform support**: Linux and Windows shell environment capture, gated macOS GUI dependencies
- **Periodic health checks** with auto-recovery for unresponsive MCP servers
- **Dynamic tool refresh** on `notifications/tools/list_changed` from child servers
- **Tool search and filter** in the dashboard
- **Built-in templates** for common MCP server configurations, with tooltip system and form validation
- **Server disabled flag** — non-disabled servers auto-start; disabled servers persist in config without running
- **Memory usage display** in the dashboard with periodic polling
- **Version display** in the dashboard UI
- **Rich tool/server info**: tool annotations (read_only, destructive, idempotent, open_world hints), MCP peer info with capability badges
- **Cargo workspace** with three crates: `mcpsm-core` (library), `mcpsm-cli` (binary), `mcpsm-gui` (binary)
- Gemini CLI and OpenCode dashboard configuration support
- GitHub Actions CI/CD for Linux (x86_64, aarch64), macOS (aarch64), and Windows, with tagged release workflow
- `.app` bundle build script with `LSUIElement: true` (no Dock icon)

### Fixed

- Graceful shutdown so Ctrl+C exits the CLI cleanly
- macOS GUI dependency gating and Linux memory metrics
- Suppressed `unused_mut` warning on Linux for `extra_paths`
