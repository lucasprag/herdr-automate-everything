//! The launchd agent that keeps the scheduler alive, also while the Herdr
//! app is closed. launchd restarts it when it exits, which is how a new
//! binary takes over.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Result};
use clap::Subcommand;

use crate::{lock, paths};

pub const LABEL: &str = "com.lucasprag.automate-everything";

#[derive(Subcommand)]
pub enum Action {
    /// Write the launchd agent and start it
    Install,
    /// Make sure everything is in place: this binary in ~/.local/bin, the
    /// launchd agent written and running. Does only what's missing; the
    /// panel calls it whenever it loads.
    Ensure,
    /// Stop it and remove the launchd agent
    Uninstall,
    /// Restart the scheduler
    Restart,
}

pub fn plist_path() -> PathBuf {
    paths::home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plist(exe: &str) -> String {
    let log = xml_escape(&paths::scheduler_log().display().to_string());
    let exe = xml_escape(exe);
    let home = xml_escape(&paths::home().display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{exe}</string><string>scheduler</string></array>
  <key>WorkingDirectory</key><string>{home}</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>AbandonProcessGroup</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#
    )
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Ok(Command::new("/bin/launchctl").args(args).output()?)
}

fn installed_exe() -> PathBuf {
    paths::home().join(".local/bin/automate-everything")
}

fn same_file_contents(a: &std::path::Path, b: &std::path::Path) -> bool {
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
fn start(target: &str) -> Result<bool> {
    if scheduler_running() {
        return Ok(false);
    }
    let out = launchctl(&["kickstart", target])?;
    if !out.status.success() {
        bail!("launchctl kickstart failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
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
        return "launchd never ran it (launchctl print gui/$UID/".to_string() + LABEL + ")";
    }
    lines[lines.len().saturating_sub(3)..].join(" ")
}

fn ensure() -> Result<()> {
    paths::ensure_state_dirs()?;
    let target = format!("{}/{LABEL}", domain());
    let mut did = Vec::new();

    // 1. This binary in ~/.local/bin. Swapped in atomically; a running
    //    scheduler sees the new file and exits, and launchd starts it again.
    let exe = installed_exe();
    let me = std::env::current_exe()?;
    let me_is_installed = std::fs::canonicalize(&me).ok() == std::fs::canonicalize(&exe).ok();
    if !me_is_installed && !same_file_contents(&me, &exe) {
        std::fs::create_dir_all(exe.parent().unwrap())?;
        let tmp = exe.with_file_name(".automate-everything.new");
        std::fs::copy(&me, &tmp)?;
        std::fs::rename(&tmp, &exe)?;
        did.push("installed the binary");
    }

    // 2. The launchd agent, rewritten only when it differs.
    let path = plist_path();
    let want = plist(&exe.display().to_string());
    let changed = std::fs::read_to_string(&path).ok().as_deref() != Some(want.as_str());
    if changed {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, &want)?;
        did.push("wrote the launchd agent");
    }

    // 3. Loaded and running.
    let loaded = launchctl(&["print", &target])?.status.success();
    if changed || !loaded {
        let _ = launchctl(&["bootout", &target]);
        let out = launchctl(&["bootstrap", &domain(), &path.display().to_string()])?;
        if !out.status.success() {
            bail!("launchctl bootstrap failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        did.push("loaded the launchd agent");
    }
    if start(&target)? {
        did.push("started the scheduler");
    }
    println!("{}", if did.is_empty() { "already installed".to_string() } else { did.join(", ") });
    Ok(())
}

pub fn run(action: Action) -> Result<()> {
    let target = format!("{}/{LABEL}", domain());
    match action {
        Action::Ensure => ensure()?,
        Action::Install => {
            paths::ensure_state_dirs()?;
            let exe = paths::home().join(".local/bin/automate-everything");
            if !exe.exists() {
                bail!("{} is missing; run ./install.sh", exe.display());
            }
            let path = plist_path();
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, plist(&exe.display().to_string()))?;
            let _ = launchctl(&["bootout", &target]);
            let out = launchctl(&["bootstrap", &domain(), &path.display().to_string()])?;
            if !out.status.success() {
                bail!("launchctl bootstrap failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            start(&target)?;
            println!("installed {} ({})", LABEL, path.display());
        }
        Action::Uninstall => {
            let _ = launchctl(&["bootout", &target]);
            let path = plist_path();
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            println!("uninstalled {LABEL}");
        }
        Action::Restart => {
            let out = launchctl(&["kickstart", "-k", &target])?;
            if !out.status.success() {
                bail!("launchctl kickstart failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            println!("restarted {LABEL}");
        }
    }
    Ok(())
}
