use std::sync::Arc;

use crate::bridge::commands::{AppCommand, BackendEvent};
use crate::core::manager::SharedServers;

/// Shared application state accessible by all axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub cmd_tx: tokio::sync::mpsc::UnboundedSender<AppCommand>,
    pub evt_tx: tokio::sync::broadcast::Sender<BackendEvent>,
    pub servers: SharedServers,
}

impl AppState {
    pub fn new(
        cmd_tx: tokio::sync::mpsc::UnboundedSender<AppCommand>,
        evt_tx: tokio::sync::broadcast::Sender<BackendEvent>,
        servers: SharedServers,
    ) -> Arc<Self> {
        Arc::new(Self {
            cmd_tx,
            evt_tx,
            servers,
        })
    }
}
