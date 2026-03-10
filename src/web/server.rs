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
        .route("/api/servers/{id}", delete(handlers::delete_server))
        .route("/api/servers/{id}/logs", get(handlers::get_logs))
        .route("/api/servers/{id}/logs", delete(handlers::clear_logs))
        .route("/api/templates", get(handlers::list_templates))
        .route("/api/events", get(sse::event_stream))
        .with_state(state)
        // Mount rmcp's StreamableHttpService at /mcp
        .nest_service("/mcp", mcp_service)
}

pub async fn serve(state: Arc<AppState>, proxy_handler: ProxyHandler) {
    let app = build_router(state, proxy_handler);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:17532")
        .await
        .expect("Failed to bind web server to 127.0.0.1:17532");
    tracing::info!("Web dashboard at http://127.0.0.1:17532");
    tracing::info!("MCP proxy (Streamable HTTP) at http://127.0.0.1:17532/mcp");
    axum::serve(listener, app).await.expect("Web server error");
}
