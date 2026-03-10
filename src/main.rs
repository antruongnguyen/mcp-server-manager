mod bridge;
mod core;
mod gui;
mod mcp;
mod web;

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::manager::{ServerManager, SharedMcpClients};
use crate::mcp::proxy::ProxyHandler;
use crate::web::state::AppState;
use tokio::sync::RwLock;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("MCPSM starting");

    // GUI/Web → Backend: tokio unbounded channel
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    // Backend → Web: broadcast channel
    let (evt_tx, _evt_rx) = tokio::sync::broadcast::channel(256);

    // Shared server state for the web layer
    let shared_servers = Arc::new(RwLock::new(HashMap::new()));

    // Shared MCP clients for the proxy
    let shared_mcp_clients: SharedMcpClients = Arc::new(RwLock::new(HashMap::new()));

    // Tool change watch channel
    let (tool_change_tx, _tool_change_rx) = tokio::sync::watch::channel(());

    // Build the web app state
    let cmd_tx_clone = cmd_tx.clone();
    let evt_tx_clone = evt_tx.clone();
    let shared_servers_clone = shared_servers.clone();
    let shared_mcp_clients_clone = shared_mcp_clients.clone();

    // Spawn tokio runtime on a background thread
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async {
            // Create the proxy handler (uses shared state via Arc)
            let proxy_handler = ProxyHandler::new(
                shared_servers_clone.clone(),
                shared_mcp_clients_clone.clone(),
            );

            let app_state = AppState::new(
                cmd_tx_clone,
                evt_tx_clone,
                shared_servers_clone,
            );

            // Run the server manager and web server concurrently
            let mut manager = ServerManager::new(
                cmd_rx,
                evt_tx,
                shared_servers,
                shared_mcp_clients,
                tool_change_tx,
            );
            tokio::select! {
                _ = manager.run() => {
                    tracing::info!("ServerManager exited");
                }
                _ = web::server::serve(app_state, proxy_handler) => {
                    tracing::info!("Web server exited");
                }
            }
        });
        tracing::info!("Backend thread exiting");
    });

    // Run the status bar on the main thread (blocks until app quits)
    let mtm = objc2_foundation::MainThreadMarker::new()
        .expect("must be called on the main thread");

    gui::status_bar::run_status_bar(mtm);
}
