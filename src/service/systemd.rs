//! Linux: a systemd user service in `$XDG_CONFIG_HOME/systemd/user`. It
//! runs while the user's systemd instance does: from login to logout, or
//! always when lingering is on (`loginctl enable-linger`).

use std::path::PathBuf;
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};

use super::LABEL;
use crate::paths;

pub const KIND: &str = "systemd user service";

fn unit_name() -> String {
    format!("{LABEL}.service")
}

pub fn unit_path() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home().join(".config"));
    config.join("systemd/user").join(unit_name())
}

/// A value in a unit file: `%` starts a specifier, so it's doubled.
fn unit_escape(s: &str) -> String {
    s.replace('%', "%%")
}

/// A word on an `ExecStart=` line, quoted.
fn exec_quote(s: &str) -> String {
    let s = unit_escape(s).replace('\\', "\\\\").replace('"', "\\\"").replace('$', "$$");
    format!("\"{s}\"")
}

pub fn unit(exe: &str) -> String {
    let exe = exec_quote(exe);
    let log = unit_escape(&paths::scheduler_log().display().to_string());
    format!(
        r#"# Written by `automate-everything service install`; `service ensure` rewrites it.
[Unit]
Description=automate-everything scheduler
StartLimitIntervalSec=0

[Service]
ExecStart={exe} scheduler
WorkingDirectory=~
Restart=always
RestartSec=10
# Runs the scheduler started live in their own session and outlive a restart
# or an update, as with launchd's AbandonProcessGroup on macOS.
KillMode=process
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
"#
    )
}

/// The command for the user's systemd instance. Over ssh or from a service
/// `XDG_RUNTIME_DIR` can be unset, and `systemctl --user` needs it to find the
/// user bus.
fn with_user_bus(program: &str) -> Command {
    let mut cmd = Command::new(program);
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        let dir = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
        if dir.is_dir() {
            cmd.env("XDG_RUNTIME_DIR", dir);
        }
    }
    cmd
}

fn systemctl(args: &[&str]) -> Result<Output> {
    with_user_bus("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("running systemctl --user (the scheduler needs systemd on Linux)")
}

fn systemctl_ok(args: &[&str]) -> Result<()> {
    let out = systemctl(args)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("Failed to connect to") || err.contains("No medium found") {
            bail!("no systemd user instance to run the scheduler: {}", err.trim());
        }
        bail!("systemctl --user {} failed: {}", args.join(" "), err.trim());
    }
    Ok(())
}

pub fn is_loaded() -> Result<bool> {
    Ok(systemctl(&["is-enabled", "--quiet", &unit_name()])?.status.success())
}

/// Makes systemd read the (re)written unit and enable it; a running
/// scheduler restarts so it runs under the new unit.
pub fn load() -> Result<()> {
    systemctl_ok(&["daemon-reload"])?;
    systemctl_ok(&["enable", &unit_name()])?;
    systemctl_ok(&["try-restart", &unit_name()])?;
    Ok(())
}

/// Starts the service now.
pub fn kick() -> Result<()> {
    systemctl_ok(&["start", &unit_name()])
}

pub fn restart() -> Result<()> {
    systemctl_ok(&["restart", &unit_name()])
}

pub fn uninstall() -> Result<()> {
    let _ = systemctl(&["disable", "--now", &unit_name()]);
    let path = unit_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok(())
}

pub fn never_ran_hint() -> String {
    format!("systemd never ran it (systemctl --user status {})", unit_name())
}

fn user_name() -> String {
    crate::env::passwd_field(|pw| pw.pw_name).or_else(|| std::env::var("USER").ok()).unwrap_or_else(|| "$USER".into())
}

/// Lingering off means the user's systemd instance, and the scheduler with
/// it, stops at logout. Turning it on can need root, so this only says how.
pub fn warnings() -> Vec<String> {
    let uid = unsafe { libc::getuid() }.to_string();
    let out = with_user_bus("loginctl").args(["show-user", &uid, "--property=Linger", "--value"]).output();
    match out {
        Ok(o) if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "no" => vec![format!(
            "lingering is off, so the scheduler stops when you log out; to keep it running: loginctl enable-linger {}",
            user_name()
        )],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_quote_escapes_specifiers_quotes_and_variables() {
        assert_eq!(exec_quote("/home/a b/x"), "\"/home/a b/x\"");
        assert_eq!(exec_quote("/x/100%/\"q\"/$HOME"), "\"/x/100%%/\\\"q\\\"/$$HOME\"");
    }

    #[test]
    fn unit_runs_the_scheduler_and_restarts_it() {
        let u = unit("/home/me/.local/bin/automate-everything");
        assert!(u.contains("ExecStart=\"/home/me/.local/bin/automate-everything\" scheduler\n"));
        assert!(u.contains("Restart=always\n"));
        assert!(u.contains("KillMode=process\n"));
        assert!(u.contains("WantedBy=default.target\n"));
    }
}
