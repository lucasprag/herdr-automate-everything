//! One run of one automation, in its own process. It holds the automation's
//! lock for as long as the run lasts (for an agent, until the agent is done
//! or its space is closed), which is what the overlap guard checks.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::config::{self, Automation, Kind, Slot};
use crate::history::{Run, Status, Trigger};
use crate::host::Host;
use crate::lock::{self, FileLock};
use crate::{agent, env, paths, proc};

pub struct Request {
    pub name: String,
    pub trigger: Trigger,
    pub run_id: String,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub caught_up: Option<u32>,
}

/// What a run sets on top of the login environment: the entry's `env` and
/// the `HERDR_AUTOMATION*` variables.
pub fn run_vars(a: &Automation, run: &Run) -> BTreeMap<String, String> {
    let mut e = a.env();
    e.insert("HERDR_AUTOMATION".into(), a.name.clone());
    e.insert("HERDR_AUTOMATION_RUN_ID".into(), run.id.clone());
    e.insert("HERDR_AUTOMATION_TRIGGER".into(), run.trigger.as_str().into());
    e
}

/// The labels on every terminal a run starts, which closing and reaping
/// look for.
pub fn run_labels(a: &Automation, run: &Run) -> Vec<(String, String)> {
    vec![("automation".to_string(), a.name.clone()), (agent::RUN_LABEL.to_string(), run.id.clone())]
}

/// This Mac's login environment plus `run_vars`.
pub fn run_env(a: &Automation, run: &Run) -> BTreeMap<String, String> {
    let mut e = env::login_env();
    e.extend(run_vars(a, run));
    e
}

pub fn shell_command(line: &str, cwd: &std::path::Path, env: &BTreeMap<String, String>) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(line).current_dir(cwd).env_clear().envs(env);
    cmd
}

pub fn execute(req: Request) -> Result<Run> {
    paths::ensure_state_dirs()?;
    let cfg = config::load();
    let slot = cfg.as_ref().ok().and_then(|c| c.find(&req.name).cloned());
    let kind = match &slot {
        Some(Slot::Valid(a)) => a.kind.as_str().to_string(),
        Some(Slot::Invalid(i)) => i.kind.clone().unwrap_or_default(),
        None => String::new(),
    };
    let mut run = Run::new(req.run_id.clone(), &req.name, &kind, req.trigger, Status::Running);
    run.scheduled_for = req.scheduled_for;
    run.caught_up = req.caught_up.filter(|n| *n > 0);
    run.runner_pid = Some(std::process::id());
    if let Some(Slot::Valid(a)) = &slot {
        run.machine = a.machine().map(String::from);
    }

    let automation = match (cfg, slot) {
        (Err(e), _) => {
            run.note = Some(format!("config: {e:#}"));
            run.finish(Status::Failed);
            run.save()?;
            return Ok(run);
        }
        (_, None) => {
            run.note = Some(format!("no automation named `{}`", req.name));
            run.finish(Status::Failed);
            run.save()?;
            return Ok(run);
        }
        (_, Some(Slot::Invalid(i))) => {
            run.note = Some(format!("invalid entry: {}", i.error));
            run.finish(Status::Failed);
            run.save()?;
            return Ok(run);
        }
        (_, Some(Slot::Valid(a))) => a,
    };

    let lock_path = paths::automation_lock(&automation.name);
    let Some(mut held) = FileLock::try_acquire(&lock_path)? else {
        let holder = lock::read_note(&lock_path).unwrap_or_default();
        run.note = Some(if holder.is_empty() {
            "still running".into()
        } else {
            format!("still running: {holder}")
        });
        run.finish(Status::SkippedOverlap);
        run.save()?;
        return Ok(run);
    };
    held.note(&format!("{} (pid {})", run.id, std::process::id()));

    run.started_at = Some(Utc::now());
    run.save()?;

    // Reaching the machine is the run's first step: an unreachable one fails
    // the run with that as its note.
    let result = Host::connect(automation.machine()).and_then(|host| match automation.kind {
        Kind::Script => run_script(&automation, &host, &mut run),
        Kind::Agent => agent::run(&automation, &host, &mut run),
    });
    if let Err(e) = result {
        run.note = Some(match run.note.take() {
            Some(n) => format!("{n}; {e:#}"),
            None => format!("{e:#}"),
        });
        if run.status.is_live() {
            run.finish(Status::Failed);
        }
    }
    run.save()?;
    drop(held);
    Ok(run)
}

fn run_script(a: &Automation, host: &Host, run: &mut Run) -> Result<()> {
    let command = a.entry.command.clone().unwrap_or_default();
    let cwd = a.script_cwd(host);
    let timeout = Duration::from_secs(a.timeout_seconds());
    let dir = run.dir();
    let out = match host {
        Host::Local => {
            let cmd = shell_command(&command, &cwd, &run_env(a, run));
            proc::run_to_files(cmd, &dir.join("stdout.log"), &dir.join("stderr.log"), timeout)?
        }
        Host::Remote(r) => {
            let r = r.run_command(&run.id, &cwd, &command, &run_vars(a, run), timeout, &run_labels(a, run))?;
            let c = r.out;
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("stdout.log"), &c.stdout)?;
            std::fs::write(dir.join("stderr.log"), &c.stderr)?;
            if let Some(e) = r.error {
                run.note = Some(e);
                run.finish(Status::Failed);
                return Ok(());
            }
            proc::Outcome { exit_code: c.exit_code, signal: c.signal, timed_out: c.timed_out }
        }
    };
    run.exit_code = out.exit_code;
    run.signal = out.signal;
    run.timed_out = out.timed_out;
    if out.timed_out {
        run.note = Some(match host.machine() {
            Some(m) => format!("stopped on `{m}` after the {} timeout", agent::human_secs(a.timeout_seconds())),
            None => format!("stopped after the {} timeout", agent::human_secs(a.timeout_seconds())),
        });
    }
    let ok = out.exit_code == Some(0) && !out.timed_out;
    run.finish(if ok { Status::Succeeded } else { Status::Failed });
    Ok(())
}

/// Starts `automate-everything _run ...` in its own session, so it outlives
/// whoever started it (the scheduler restarting on a new binary, the panel).
pub fn spawn(req: &Request) -> Result<std::process::Child> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("_run")
        .arg(&req.name)
        .arg("--run-id")
        .arg(&req.run_id)
        .arg("--trigger")
        .arg(req.trigger.as_str());
    if let Some(t) = req.scheduled_for {
        cmd.arg("--scheduled-for").arg(t.to_rfc3339());
    }
    if let Some(n) = req.caught_up {
        cmd.arg("--caught-up").arg(n.to_string());
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::state_dir().join("runner.log"))?;
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .current_dir(paths::home());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    Ok(cmd.spawn()?)
}
