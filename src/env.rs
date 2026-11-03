//! The user's login environment. launchd starts us with a bare PATH, so
//! scripts, prechecks and agents get the environment a login shell builds
//! instead, cached for a few minutes.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::paths;
use crate::proc;

const CACHE_FOR: Duration = Duration::from_secs(10 * 60);
const MARKER: &str = "__AUTOMATE_EVERYTHING_ENV__";

/// Variables a login shell sets that describe that shell, not the user.
const DROP: &[&str] = &["_", "SHLVL", "PWD", "OLDPWD", "TERM_SESSION_ID", "HC_TERMINAL_ID"];

fn from_shell() -> Option<BTreeMap<String, String>> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let mut cmd = Command::new(&shell);
    cmd.args(["-l", "-i", "-c", &format!("printf '\\0{MARKER}\\0'; /usr/bin/env -0")])
        .current_dir(paths::home())
        .stdin(Stdio::null());
    let out = proc::output_with_timeout(cmd, Duration::from_secs(20)).ok()?;
    if out.timed_out {
        return None;
    }
    let marker = format!("\0{MARKER}\0");
    let idx = out.stdout.windows(marker.len()).position(|w| w == marker.as_bytes())?;
    let body = &out.stdout[idx + marker.len()..];
    let mut env = BTreeMap::new();
    for chunk in body.split(|b| *b == 0) {
        let s = String::from_utf8_lossy(chunk);
        if let Some((k, v)) = s.split_once('=') {
            if !k.is_empty() && !DROP.contains(&k) {
                env.insert(k.to_string(), v.to_string());
            }
        }
    }
    if env.contains_key("PATH") {
        Some(env)
    } else {
        None
    }
}

fn fallback() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    let home = paths::home();
    let extra = [
        home.join(".local/bin").display().to_string(),
        home.join(".asdf/shims").display().to_string(),
        "/opt/homebrew/bin".into(),
        "/opt/homebrew/sbin".into(),
        "/usr/local/bin".into(),
    ];
    let current = env.get("PATH").cloned().unwrap_or_else(|| "/usr/bin:/bin:/usr/sbin:/sbin".into());
    let mut parts: Vec<String> = extra.to_vec();
    parts.extend(current.split(':').map(String::from));
    let mut seen = std::collections::HashSet::new();
    parts.retain(|p| seen.insert(p.clone()));
    env.insert("PATH".into(), parts.join(":"));
    env
}

pub fn login_env() -> BTreeMap<String, String> {
    let cache = paths::login_env_cache();
    if let Ok(meta) = std::fs::metadata(&cache) {
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .map(|age| age < CACHE_FOR)
            .unwrap_or(false);
        if fresh {
            if let Ok(text) = std::fs::read_to_string(&cache) {
                if let Ok(env) = serde_json::from_str::<BTreeMap<String, String>>(&text) {
                    return env;
                }
            }
        }
    }
    match from_shell() {
        Some(env) => {
            let _ = paths::ensure_state_dirs();
            let tmp = cache.with_extension(format!("tmp.{}", std::process::id()));
            // It holds whatever the login shell exports, tokens included: owner only.
            let written = {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&tmp)
                    .and_then(|mut f| f.write_all(serde_json::to_string(&env).unwrap_or_default().as_bytes()))
            };
            if written.is_ok() {
                let _ = std::fs::rename(&tmp, &cache);
            }
            env
        }
        None => fallback(),
    }
}
