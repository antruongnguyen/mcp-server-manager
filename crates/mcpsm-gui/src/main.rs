mod gui;

use std::sync::Arc;

use mcpsm_core::core::{config, shell_env, watcher};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("MCPSM starting");

    // Capture the user's full shell environment (before tokio runtime).
    // This is critical for .app bundles which don't inherit the user's PATH.
    let shell_env = Arc::new(shell_env::capture_shell_env());

    // Early config read to extract port
    let port = config::load()
        .map(|(_, doc)| config::extract_port(&doc))
        .unwrap_or(config::DEFAULT_PORT);

    // GUI/Web → Backend: tokio unbounded channel
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    // Set up status bar communication before spawning background thread
    gui::status_bar::set_cmd_tx(cmd_tx.clone());
    gui::status_bar::set_port(port);

    // Spawn config file watcher
    if let Ok(path) = config::config_path() {
        watcher::spawn_config_watcher(path, cmd_tx.clone());
    }

    // Spawn tokio runtime on a background thread
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(mcpsm_core::runtime::run_backend(port, cmd_rx, cmd_tx, shell_env));
        tracing::info!("Backend thread exiting");
    });

    // Run the status bar on the main thread (blocks until app quits)
    let mtm = objc2_foundation::MainThreadMarker::new()
        .expect("must be called on the main thread");

    gui::status_bar::run_status_bar(mtm);
}
