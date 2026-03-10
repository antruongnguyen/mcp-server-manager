use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Response};

use crate::bridge::commands::AppCommand;
use crate::core::server::ServerConfig;
use crate::core::templates::builtin_templates;

use super::state::AppState;

const DASHBOARD_HTML: &str = include_str!("dashboard.html");

pub async fn dashboard() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        DASHBOARD_HTML,
    )
        .into_response()
}

pub async fn list_servers(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let servers = state.servers.read().await;
    Json(serde_json::to_value(&*servers).unwrap_or_default())
}

#[derive(serde::Deserialize)]
pub struct AddServerRequest {
    pub id: String,
    pub config: ServerConfig,
}

pub async fn add_server(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AddServerRequest>,
) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::AddServer {
        id: body.id,
        config: body.config,
    });
    StatusCode::ACCEPTED
}

pub async fn start_server(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::StartServer { id });
    StatusCode::ACCEPTED
}

pub async fn stop_server(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::StopServer { id });
    StatusCode::ACCEPTED
}

pub async fn delete_server(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::DeleteServer { id });
    StatusCode::ACCEPTED
}

pub async fn get_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::RequestLogs { id });
    StatusCode::ACCEPTED
}

pub async fn list_templates() -> Json<serde_json::Value> {
    let templates = builtin_templates();
    let list: Vec<serde_json::Value> = templates
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "config": t.config,
            })
        })
        .collect();
    Json(serde_json::Value::Array(list))
}
