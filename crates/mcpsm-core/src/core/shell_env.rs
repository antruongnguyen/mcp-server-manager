use std::collections::HashMap;

/// Capture the user's full shell environment by running `$SHELL -l -c env`.
///
/// macOS `.app` bundles launched from Finder don't inherit the user's shell
/// PATH, which breaks tools installed via nvm, pyenv, Homebrew, etc.
/// This runs the user's login shell once at startup to capture the real
/// environment, then child processes inherit it.
pub fn capture_shell_env() -> HashMap<String, String> {
    match try_capture() {
        Ok(env) => {
            tracing::info!(
                "Captured shell environment ({} vars, PATH has {} entries)",
                env.len(),
                env.get("PATH").map(|p| p.split(':').count()).unwrap_or(0),
            );
            env
        }
        Err(e) => {
            tracing::warn!("Failed to capture shell env: {}; using fallback", e);
            build_fallback_env()
        }
    }
}

fn try_capture() -> Result<HashMap<String, String>, String> {
    let shell = std::env::var("SHELL").map_err(|_| "$SHELL not set".to_string())?;

    let (arg_flag, cmd_flag) = if shell.ends_with("/fish") {
        ("--login", "-c")
    } else {
        ("-l", "-c")
    };

    let output = std::process::Command::new(&shell)
        .args([arg_flag, cmd_flag, "env"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run {}: {}", shell, e))?;

    if !output.status.success() {
        return Err(format!(
            "{} exited with {}",
            shell,
            output.status.code().unwrap_or(-1)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let env: HashMap<String, String> = stdout
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            // Skip empty keys and shell internal vars
            if key.is_empty() || key.contains(' ') {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect();

    // Sanity checks
    if !env.contains_key("PATH") {
        return Err("Captured env has no PATH".into());
    }
    if env.len() < 5 {
        return Err(format!("Captured env too small ({} vars)", env.len()));
    }

    Ok(env)
}

/// Fallback environment replicating the old `augment_path()` behavior
/// plus essential variables.
fn build_fallback_env() -> HashMap<String, String> {
    let mut env = HashMap::new();

    // Build PATH from hardcoded common dirs + current PATH
    let current_path = std::env::var("PATH").unwrap_or_default();
    let extra_paths = [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
    ];
    let mut paths: Vec<&str> = extra_paths.to_vec();
    for segment in current_path.split(':') {
        if !segment.is_empty() && !paths.contains(&segment) {
            paths.push(segment);
        }
    }
    env.insert("PATH".into(), paths.join(":"));

    // Copy essential vars from current process env
    for key in &["HOME", "USER", "TMPDIR", "LANG", "SHELL", "TERM"] {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }

    env
}
