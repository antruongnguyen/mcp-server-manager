use std::collections::HashMap;

/// Platform-specific PATH separator.
#[cfg(unix)]
const PATH_SEP: char = ':';
#[cfg(windows)]
const PATH_SEP: char = ';';

/// Capture the user's full shell environment.
///
/// On Unix (macOS/Linux), runs `$SHELL -l -c env` to capture the login shell
/// environment. macOS `.app` bundles launched from Finder don't inherit the
/// user's shell PATH, which breaks tools installed via nvm, pyenv, Homebrew, etc.
///
/// On Windows, shell capture is skipped — the process environment is inherited
/// directly, since Windows doesn't have the login-shell divergence problem.
pub fn capture_shell_env() -> HashMap<String, String> {
    match try_capture() {
        Ok(env) => {
            tracing::info!(
                "Captured shell environment ({} vars, PATH has {} entries)",
                env.len(),
                env.get("PATH")
                    .map(|p| p.split(PATH_SEP).count())
                    .unwrap_or(0),
            );
            env
        }
        Err(e) => {
            tracing::warn!("Failed to capture shell env: {}; using fallback", e);
            build_fallback_env()
        }
    }
}

/// Ensure `path_dir` is present in the PATH value within `env`.
/// Uses the platform-appropriate separator.
pub fn ensure_path_has(env: &mut HashMap<String, String>, path_dir: &str) {
    let path = env.entry("PATH".into()).or_default();
    let already_present = path.split(PATH_SEP).any(|seg| seg == path_dir);
    if !already_present {
        if !path.is_empty() {
            path.push(PATH_SEP);
        }
        path.push_str(path_dir);
    }
}

// ---------------------------------------------------------------------------
// Unix: capture via login shell
// ---------------------------------------------------------------------------
#[cfg(unix)]
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

// ---------------------------------------------------------------------------
// Windows: inherit process environment directly
// ---------------------------------------------------------------------------
#[cfg(windows)]
fn try_capture() -> Result<HashMap<String, String>, String> {
    let env: HashMap<String, String> = std::env::vars().collect();

    if !env.contains_key("PATH") && !env.contains_key("Path") {
        return Err("Process env has no PATH".into());
    }
    if env.len() < 5 {
        return Err(format!("Process env too small ({} vars)", env.len()));
    }

    // Normalize: Windows PATH is case-insensitive, but we store as "PATH"
    let mut result = env;
    if !result.contains_key("PATH") {
        if let Some(val) = result.remove("Path") {
            result.insert("PATH".into(), val);
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Fallback: Unix
// ---------------------------------------------------------------------------
#[cfg(unix)]
fn build_fallback_env() -> HashMap<String, String> {
    let mut env = HashMap::new();

    // Build PATH from common dirs + current PATH
    let current_path = std::env::var("PATH").unwrap_or_default();

    let mut extra_paths: Vec<&str> = vec!["/usr/local/bin"];

    // Homebrew paths are macOS-only
    #[cfg(target_os = "macos")]
    {
        extra_paths.push("/opt/homebrew/bin");
        extra_paths.push("/opt/homebrew/sbin");
    }

    let mut paths: Vec<&str> = extra_paths.to_vec();
    for segment in current_path.split(PATH_SEP) {
        if !segment.is_empty() && !paths.contains(&segment) {
            paths.push(segment);
        }
    }
    env.insert("PATH".into(), paths.join(&PATH_SEP.to_string()));

    // Copy essential Unix vars from current process env
    for key in &["HOME", "USER", "TMPDIR", "LANG", "SHELL", "TERM"] {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }

    env
}

// ---------------------------------------------------------------------------
// Fallback: Windows
// ---------------------------------------------------------------------------
#[cfg(windows)]
fn build_fallback_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();

    // Normalize PATH casing
    if !env.contains_key("PATH") {
        if let Some(val) = env.remove("Path") {
            env.insert("PATH".into(), val);
        }
    }

    // Ensure essential Windows vars are present
    for key in &["USERPROFILE", "USERNAME", "TEMP", "SYSTEMROOT", "COMSPEC"] {
        if !env.contains_key(*key) {
            if let Ok(val) = std::env::var(key) {
                env.insert(key.to_string(), val);
            }
        }
    }

    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_returns_nonempty_path() {
        let env = capture_shell_env();
        let path = env.get("PATH").expect("PATH must be present");
        assert!(!path.is_empty(), "PATH must not be empty");
        assert!(
            path.split(PATH_SEP).count() >= 1,
            "PATH should have at least one entry"
        );
    }

    #[test]
    fn ensure_path_has_adds_missing_dir() {
        let mut env = HashMap::new();
        env.insert("PATH".into(), format!("/usr/bin{}/bin", PATH_SEP));

        ensure_path_has(&mut env, "/opt/new");
        let path = env.get("PATH").unwrap();
        assert!(path.split(PATH_SEP).any(|s| s == "/opt/new"));
    }

    #[test]
    fn ensure_path_has_skips_existing_dir() {
        let mut env = HashMap::new();
        env.insert("PATH".into(), format!("/usr/bin{}/bin", PATH_SEP));

        let original = env.get("PATH").unwrap().clone();
        ensure_path_has(&mut env, "/usr/bin");
        assert_eq!(env.get("PATH").unwrap(), &original);
    }

    #[test]
    fn ensure_path_has_handles_empty_path() {
        let mut env = HashMap::new();
        ensure_path_has(&mut env, "/usr/local/bin");
        assert_eq!(env.get("PATH").unwrap(), "/usr/local/bin");
    }
}
