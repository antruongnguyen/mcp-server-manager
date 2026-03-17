use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::bridge::commands::AppCommand;
use crate::core::manager::{ServerManager, SharedMcpClients};
use crate::mcp::proxy::ProxyHandler;
use crate::web::state::AppState;

/// Run the full backend: ServerManager + web server + MCP proxy.
///
/// This is the shared async entry point used by both the CLI and GUI binaries.
/// The caller is responsible for creating the tokio runtime and the command channel.
pub async fn run_backend(
    port: u16,
    cmd_rx: tokio::sync::mpsc::UnboundedReceiver<AppCommand>,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<AppCommand>,
    shell_env: Arc<HashMap<String, String>>,
) {
    // Backend → Web: broadcast channel
    let (evt_tx, _evt_rx) = tokio::sync::broadcast::channel(256);

    // Shared server state for the web layer
    let shared_servers = Arc::new(RwLock::new(HashMap::new()));

    // Shared MCP clients for the proxy
    let shared_mcp_clients: SharedMcpClients = Arc::new(RwLock::new(HashMap::new()));

    // Tool change watch channel
    let (tool_change_tx, _tool_change_rx) = tokio::sync::watch::channel(());

    // Signal handler for graceful shutdown (Ctrl+C)
    let signal_tx = cmd_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Ctrl+C received, shutting down...");
        let _ = signal_tx.send(AppCommand::Shutdown);
    });

    // Create the proxy handler (uses shared state via Arc)
    let proxy_handler = ProxyHandler::new(
        shared_servers.clone(),
        shared_mcp_clients.clone(),
    );

    let app_state = AppState::new(
        cmd_tx,
        evt_tx.clone(),
        shared_servers.clone(),
    );

    // Run the server manager and web server concurrently
    let mut manager = ServerManager::new(
        cmd_rx,
        evt_tx,
        shared_servers,
        shared_mcp_clients,
        tool_change_tx,
        port,
        shell_env,
    );
    tokio::select! {
        _ = manager.run() => {
            tracing::info!("ServerManager exited");
        }
        _ = crate::web::server::serve(app_state, proxy_handler, port) => {
            tracing::info!("Web server exited");
        }
    }
}
