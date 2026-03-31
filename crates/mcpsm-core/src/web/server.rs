use std::sync::Arc;

use axum::routing::{delete, get, post};
use axum::Router;

use super::handlers;
use super::sse;
use super::state::AppState;
use crate::mcp::proxy::ProxyHandler;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

pub fn build_router(state: Arc<AppState>, proxy_handler: ProxyHandler) -> Router {
    let mcp_service = StreamableHttpService::new(
        move || Ok(proxy_handler.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    Router::new()
        .route("/", get(handlers::dashboard))
        .route("/api/servers", get(handlers::list_servers))
        .route("/api/servers", post(handlers::add_server))
        .route("/api/servers/{id}/start", post(handlers::start_server))
        .route("/api/servers/{id}/stop", post(handlers::stop_server))
        .route(
            "/api/servers/{id}",
            delete(handlers::delete_server).put(handlers::update_server),
        )
        .route("/api/servers/{id}/logs", get(handlers::get_logs))
        .route("/api/servers/{id}/logs", delete(handlers::clear_logs))
        .route(
            "/api/servers/{id}/log-level",
            post(handlers::set_log_level),
        )
        .route(
            "/api/servers/{id}/disabled",
            post(handlers::set_disabled),
        )
        .route("/api/servers/start-all", post(handlers::start_all))
        .route("/api/servers/stop-all", post(handlers::stop_all))
        .route("/api/memory", get(handlers::get_memory))
        .route("/api/templates", get(handlers::list_templates))
        .route("/api/events", get(sse::event_stream))
        .with_state(state)
        // Mount rmcp's StreamableHttpService at /mcp
        .nest_service("/mcp", mcp_service)
}

pub async fn serve(
    state: Arc<AppState>,
    proxy_handler: ProxyHandler,
    port: u16,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let app = build_router(state, proxy_handler);
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|_| panic!("Failed to bind web server to {}", addr));
    tracing::info!("Web dashboard at http://{}", addr);
    tracing::info!("MCP proxy (Streamable HTTP) at http://{}/mcp", addr);

    // Auto-open dashboard in the default browser
    let url = format!("http://{}", addr);
    tokio::task::spawn_blocking(move || {
        let _ = open::that(&url);
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
            tracing::info!("Web server received shutdown signal");
        })
        .await
        .expect("Web server error");
}
