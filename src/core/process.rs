use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;

use crate::core::server::ServerConfig;

/// Gracefully stop a child process: SIGTERM, wait, then SIGKILL if needed.
pub async fn stop_server(child: &mut Child) {
    // Try SIGTERM first (on Unix)
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }

    // Wait up to 5 seconds for graceful shutdown
    match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
        Ok(_) => return,
        Err(_) => {
            tracing::warn!("Server did not stop within 5s, sending SIGKILL");
            let _ = child.kill().await;
        }
    }
}

/// Spawn a task that reads lines from a child's stderr and forwards
/// them via a channel.
pub fn spawn_stderr_reader(
    stderr: tokio::process::ChildStderr,
    id: String,
    tx: tokio::sync::mpsc::UnboundedSender<(String, String)>,
) {
    tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send((id.clone(), line)).is_err() {
                break;
            }
        }
    });
}

/// Get the PID from a child process, if available.
pub fn get_pid(child: &Child) -> Option<u32> {
    child.id()
}

/// Check if a config is for a stdio (command-based) server.
#[allow(dead_code)]
pub fn is_stdio_config(config: &ServerConfig) -> bool {
    config.command.is_some()
}
