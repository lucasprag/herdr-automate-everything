use std::path::PathBuf;

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Expands a leading `~` or `~/`.
pub fn expand(p: &str) -> PathBuf {
    if p == "~" {
        home()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(p)
    }
}

/// `$AUTOMATE_EVERYTHING_CONFIG`, else `~/.config/automate-everything/automations.toml`.
pub fn config_file() -> PathBuf {
    if let Some(p) = std::env::var_os("AUTOMATE_EVERYTHING_CONFIG") {
        return PathBuf::from(p);
    }
    home().join(".config/automate-everything/automations.toml")
}

/// `$AUTOMATE_EVERYTHING_STATE`, else `~/.local/state/automate-everything`.
pub fn state_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("AUTOMATE_EVERYTHING_STATE") {
        return PathBuf::from(p);
    }
    home().join(".local/state/automate-everything")
}

pub fn runs_dir() -> PathBuf {
    state_dir().join("runs")
}

pub fn locks_dir() -> PathBuf {
    state_dir().join("locks")
}

pub fn automation_lock(name: &str) -> PathBuf {
    locks_dir().join(format!("{name}.lock"))
}

pub fn scheduler_lock() -> PathBuf {
    state_dir().join("scheduler.lock")
}

pub fn config_lock() -> PathBuf {
    state_dir().join("config.lock")
}

pub fn cursors_file() -> PathBuf {
    state_dir().join("cursors.json")
}

pub fn scheduler_log() -> PathBuf {
    state_dir().join("scheduler.log")
}

pub fn login_env_cache() -> PathBuf {
    state_dir().join("login-env.json")
}

pub fn ensure_state_dirs() -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let state = state_dir();
    if !state.exists() {
        std::fs::create_dir_all(&state)?;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::create_dir_all(runs_dir())?;
    std::fs::create_dir_all(locks_dir())?;
    Ok(())
}
