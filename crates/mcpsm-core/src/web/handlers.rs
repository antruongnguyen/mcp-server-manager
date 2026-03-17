use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Response};

use crate::bridge::commands::AppCommand;
use crate::core::server::{ServerConfig, ServerStatus};
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

#[derive(serde::Deserialize)]
pub struct UpdateServerRequest {
    pub config: ServerConfig,
}

pub async fn update_server(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateServerRequest>,
) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::UpdateServer {
        id,
        config: body.config,
    });
    StatusCode::ACCEPTED
}

pub async fn get_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::RequestLogs { id });
    StatusCode::ACCEPTED
}

pub async fn clear_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::ClearLogs { id });
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

#[derive(serde::Deserialize)]
pub struct SetDisabledRequest {
    pub disabled: bool,
}

pub async fn set_disabled(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<SetDisabledRequest>,
) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::SetServerDisabled {
        id,
        disabled: body.disabled,
    });
    StatusCode::ACCEPTED
}

pub async fn start_all(State(state): State<Arc<AppState>>) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::StartAllServers);
    StatusCode::ACCEPTED
}

pub async fn stop_all(State(state): State<Arc<AppState>>) -> StatusCode {
    let _ = state.cmd_tx.send(AppCommand::StopAllServers);
    StatusCode::ACCEPTED
}

/// Get physical memory footprint in bytes for a given PID using macOS proc_pid_rusage.
///
/// This returns `ri_phys_footprint` — the same metric Activity Monitor shows in its
/// "Memory" column. It represents memory actually *charged* to the process (private
/// dirty + compressed + IOKit), excluding shared framework pages.
///
/// This differs from RSS (`pti_resident_size` via `proc_pidinfo`), which counts *all*
/// resident pages including shared libraries mapped by every process, resulting in
/// over-counting.
#[cfg(target_os = "macos")]
fn get_memory_footprint(pid: u32) -> Option<u64> {
    // rusage_info_v0 layout from <sys/resource.h>
    #[repr(C)]
    #[allow(non_camel_case_types)]
    struct rusage_info_v0 {
        ri_uuid: [u8; 16],
        ri_user_time: u64,
        ri_system_time: u64,
        ri_pkg_idle_wkups: u64,
        ri_interrupt_wkups: u64,
        ri_pageins: u64,
        ri_wired_size: u64,
        ri_resident_size: u64,
        ri_phys_footprint: u64,
        ri_proc_start_abstime: u64,
        ri_proc_exit_abstime: u64,
    }

    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut std::ffi::c_void) -> i32;
    }

    unsafe {
        let mut info: rusage_info_v0 = std::mem::zeroed();
        let ret = proc_pid_rusage(
            pid as i32,
            0, // RUSAGE_INFO_V0
            &raw mut info as *mut std::ffi::c_void,
        );
        if ret == 0 {
            Some(info.ri_phys_footprint)
        } else {
            None
        }
    }
}

pub async fn get_memory(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let self_pid = std::process::id();
    let self_mem = get_memory_footprint(self_pid).unwrap_or(0);

    let servers = state.servers.read().await;
    let mut server_memory = serde_json::Map::new();
    let mut servers_total: u64 = 0;

    for (id, info) in servers.iter() {
        let pid = match &info.status {
            ServerStatus::Running { pid } => Some(*pid),
            ServerStatus::Initializing { pid } => *pid,
            ServerStatus::Ready { pid } => *pid,
            _ => None,
        };
        if let Some(pid) = pid {
            if let Some(mem) = get_memory_footprint(pid) {
                server_memory.insert(id.clone(), serde_json::json!({ "pid": pid, "mem": mem }));
                servers_total += mem;
            }
        }
    }

    Json(serde_json::json!({
        "self": { "pid": self_pid, "mem": self_mem },
        "servers": server_memory,
        "servers_total": servers_total,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
