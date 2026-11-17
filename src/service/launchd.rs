//! macOS: a launchd agent in `~/Library/LaunchAgents`.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Result};

use super::LABEL;
use crate::paths;

pub const KIND: &str = "launchd agent";

pub fn unit_path() -> PathBuf {
    paths::home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn target() -> String {
    format!("{}/{LABEL}", domain())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn unit(exe: &str) -> String {
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

pub fn is_loaded() -> Result<bool> {
    Ok(launchctl(&["print", &target()])?.status.success())
}

/// (Re)loads the agent from its plist.
pub fn load() -> Result<()> {
    let _ = launchctl(&["bootout", &target()]);
    let out = launchctl(&["bootstrap", &domain(), &unit_path().display().to_string()])?;
    if !out.status.success() {
        bail!("launchctl bootstrap failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Starts the loaded job now.
pub fn kick() -> Result<()> {
    let out = launchctl(&["kickstart", &target()])?;
    if !out.status.success() {
        bail!("launchctl kickstart failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn restart() -> Result<()> {
    let out = launchctl(&["kickstart", "-k", &target()])?;
    if !out.status.success() {
        bail!("launchctl kickstart failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let _ = launchctl(&["bootout", &target()]);
    let path = unit_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

pub fn never_ran_hint() -> String {
    "launchd never ran it (launchctl print gui/$UID/".to_string() + LABEL + ")"
}

pub fn warnings() -> Vec<String> {
    Vec::new()
}
