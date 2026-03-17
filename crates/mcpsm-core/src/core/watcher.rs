use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge::commands::AppCommand;

/// Spawn a dedicated std::thread that watches the config file's parent directory
/// for changes. On change, sends `AppCommand::ReloadConfigIfChanged` through `cmd_tx`.
///
/// Uses a 500ms debounce window to handle atomic-write editors (write-tmp-rename).
pub fn spawn_config_watcher(config_path: PathBuf, cmd_tx: UnboundedSender<AppCommand>) {
    let Some(parent_dir) = config_path.parent().map(|p| p.to_path_buf()) else {
        tracing::warn!("Cannot watch config: no parent directory");
        return;
    };
    let file_name = config_path
        .file_name()
        .map(|n| n.to_os_string());

    std::thread::spawn(move || {
        let (tx, rx) = std_mpsc::channel::<notify::Result<Event>>();

        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("Failed to create config watcher: {}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(&parent_dir, RecursiveMode::NonRecursive) {
            tracing::error!("Failed to watch config directory: {}", e);
            return;
        }

        tracing::info!("Config file watcher active on {:?}", parent_dir);

        let debounce = Duration::from_millis(500);
        let mut last_trigger = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);

        loop {
            match rx.recv() {
                Ok(Ok(event)) => {
                    // Only care about create/modify events (covers atomic write-tmp-rename)
                    let dominated = matches!(
                        event.kind,
                        EventKind::Create(_) | EventKind::Modify(_)
                    );
                    if !dominated {
                        continue;
                    }

                    // Filter to events that affect our config file name
                    let affects_config = event.paths.iter().any(|p| {
                        match (&file_name, p.file_name()) {
                            (Some(expected), Some(actual)) => actual == expected,
                            _ => false,
                        }
                    });
                    if !affects_config {
                        continue;
                    }

                    // Debounce
                    if last_trigger.elapsed() < debounce {
                        continue;
                    }
                    last_trigger = Instant::now();

                    tracing::info!("Config file change detected");
                    let _ = cmd_tx.send(AppCommand::ReloadConfigIfChanged);
                }
                Ok(Err(e)) => {
                    tracing::warn!("Config watcher error: {}", e);
                }
                Err(_) => {
                    tracing::info!("Config watcher channel closed, exiting");
                    break;
                }
            }
        }
    });
}
