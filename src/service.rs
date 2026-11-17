//! The background service that keeps the scheduler alive, also while the
//! Herdr app is closed: a launchd agent on macOS, a systemd user service on
//! Linux. A new binary takes over by the scheduler re-executing itself; the
//! service manager may pend or delay its own spawns and respawns, so `ensure`
//! checks the scheduler is really up and kicks it when it isn't.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::Subcommand;

use crate::{lock, paths};

#[cfg(target_os = "macos")]
mod launchd;
#[cfg(target_os = "macos")]
use launchd as backend;

#[cfg(not(target_os = "macos"))]
mod systemd;
#[cfg(not(target_os = "macos"))]
use systemd as backend;

pub const LABEL: &str = "com.lucasprag.automate-everything";

#[derive(Subcommand)]
pub enum Action {
    /// Write the background service (launchd agent on macOS, systemd user
    /// service on Linux) and start it
    Install,
    /// Make sure everything is in place: this binary in ~/.local/bin, the
    /// background service written and running. Does only what's missing; the
    /// panel calls it whenever it loads.
    Ensure,
    /// Stop it and remove the background service
    Uninstall,
    /// Restart the scheduler
    Restart,
}

/// Whether the service file (launchd plist or systemd unit) is in place.
pub fn installed() -> bool {
    backend::unit_path().exists()
}

/// Things about the service the user should fix, such as systemd lingering
/// being off. Empty on macOS.
pub fn warnings() -> Vec<String> {
    backend::warnings()
}

fn installed_exe() -> PathBuf {
    paths::home().join(".local/bin/automate-everything")
}

fn same_file_contents(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn scheduler_running() -> bool {
    lock::is_held(&paths::scheduler_lock())
}

// Starts the scheduler unless it's running; true when it had to. A loaded job
// isn't necessarily running: right after a bootstrap launchd can leave a
// RunAtLoad job as a pending "speculative" spawn that never happens, so this
// kicks it and waits for its lock, which a started scheduler takes right away.
fn start() -> Result<bool> {
    if scheduler_running() {
        return Ok(false);
    }
    backend::kick()?;
    for _ in 0..20 {
        if scheduler_running() {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    bail!("the scheduler didn't start (log {}): {}", paths::scheduler_log().display(), log_tail());
}

fn log_tail() -> String {
    let log = std::fs::read_to_string(paths::scheduler_log()).unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return backend::never_ran_hint();
    }
    lines[lines.len().saturating_sub(3)..].join(" ")
}

fn print_warnings() {
    for w in warnings() {
        eprintln!("warning: {w}");
    }
}

fn ensure() -> Result<()> {
    paths::ensure_state_dirs()?;
    let mut did = Vec::new();

    // 1. This binary in ~/.local/bin. Swapped in atomically; a running
    //    scheduler sees the new file within a minute and re-executes as it.
    let exe = installed_exe();
    let me = paths::current_exe()?;
    let me_is_installed = std::fs::canonicalize(&me).ok() == std::fs::canonicalize(&exe).ok();
    if !me_is_installed && !same_file_contents(&me, &exe) {
        std::fs::create_dir_all(exe.parent().unwrap())?;
        let tmp = exe.with_file_name(".automate-everything.new");
        std::fs::copy(&me, &tmp)?;
        std::fs::rename(&tmp, &exe)?;
        did.push("installed the binary".to_string());
    }

    // 2. The service file, rewritten only when it differs.
    let path = backend::unit_path();
    let want = backend::unit(&exe.display().to_string());
    let changed = std::fs::read_to_string(&path).ok().as_deref() != Some(want.as_str());
    if changed {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, &want)?;
        did.push(format!("wrote the {}", backend::KIND));
    }

    // 3. Loaded and running.
    if changed || !backend::is_loaded()? {
        backend::load()?;
        did.push(format!("loaded the {}", backend::KIND));
    }
    if start()? {
        did.push("started the scheduler".to_string());
    }
    println!("{}", if did.is_empty() { "already installed".to_string() } else { did.join(", ") });
    print_warnings();
    Ok(())
}

pub fn run(action: Action) -> Result<()> {
    match action {
        Action::Ensure => ensure()?,
        Action::Install => {
            paths::ensure_state_dirs()?;
            let exe = installed_exe();
            if !exe.exists() {
                bail!("{} is missing; run ./install.sh", exe.display());
            }
            let path = backend::unit_path();
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, backend::unit(&exe.display().to_string()))?;
            backend::load()?;
            start()?;
            println!("installed {} ({})", LABEL, path.display());
            print_warnings();
        }
        Action::Uninstall => {
            backend::uninstall()?;
            println!("uninstalled {LABEL}");
        }
        Action::Restart => {
            backend::restart()?;
            println!("restarted {LABEL}");
        }
    }
    Ok(())
}
