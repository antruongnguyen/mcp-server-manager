use std::sync::Arc;

use mcpsm_core::core::{config, shell_env, watcher};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("MCPSM starting (headless CLI)");

    // Capture the user's full shell environment.
    let shell_env = Arc::new(shell_env::capture_shell_env());

    // Load config and extract port
    let port = config::load()
        .map(|(_, doc)| config::extract_port(&doc))
        .unwrap_or(config::DEFAULT_PORT);

    // Create the command channel
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn config file watcher
    if let Ok(path) = config::config_path() {
        watcher::spawn_config_watcher(path, cmd_tx.clone());
    }

    // Run everything on the main thread's tokio runtime
    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    println!("Dashboard: http://127.0.0.1:{port}");
    rt.block_on(mcpsm_core::runtime::run_backend(port, cmd_rx, cmd_tx, shell_env));
}
