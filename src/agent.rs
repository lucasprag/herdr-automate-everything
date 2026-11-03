//! Agent runs on herdr-core.
//!
//! precheck (scheduled runs only) → directory (repo or a new worktree) →
//! space (new, or a tab in a shared one) → launch the agent with its prompt
//! in a labelled terminal → watch it until it's done, blocked, failed or
//! closed → close only what this run opened.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde_json::Value;

use crate::config::{Automation, Directory, SpaceMode};
use crate::history::{self, AgentInfo, Run, Status, StepResult, Trigger};
use crate::runner::{run_env, shell_command};
use crate::{hc, paths, proc};

const POLL: Duration = Duration::from_secs(3);
/// How long an agent has to take its prompt.
const START_TIMEOUT: Duration = Duration::from_secs(180);
/// How long `done` must hold before the run counts as succeeded.
const SETTLE: Duration = Duration::from_secs(9);
const CAPTURE_EVERY: Duration = Duration::from_secs(60);
const OUTPUT_LINES: usize = 5000;
const PRECHECK_KEEP: usize = 64 * 1024;
pub const RUN_LABEL: &str = "automation.run";

pub fn run(a: &Automation, run: &mut Run) -> Result<()> {
    let env = run_env(a, run);
    let repo = a.repo().ok_or_else(|| anyhow!("no repo"))?;

    // 1. Precheck: exit 0 starts the run; anything else skips it.
    if run.trigger == Trigger::Scheduled {
        if let Some(pc) = a.entry.precheck.clone().filter(|p| !p.trim().is_empty()) {
            let cmd = shell_command(&pc, &repo, &env);
            let res = proc::output_with_timeout(cmd, Duration::from_secs(a.precheck_timeout_seconds()));
            let step = match res {
                Ok(o) => StepResult {
                    command: pc.clone(),
                    exit_code: o.exit_code,
                    signal: o.signal,
                    timed_out: o.timed_out,
                    duration_ms: o.duration.as_millis() as u64,
                    stdout: proc::tail_string(&o.stdout, PRECHECK_KEEP),
                    stderr: proc::tail_string(&o.stderr, PRECHECK_KEEP),
                },
                Err(e) => StepResult {
                    command: pc.clone(),
                    exit_code: None,
                    signal: None,
                    timed_out: false,
                    duration_ms: 0,
                    stdout: String::new(),
                    stderr: format!("could not start: {e}"),
                },
            };
            let passed = step.exit_code == Some(0) && !step.timed_out;
            run.precheck = Some(step);
            if !passed {
                run.finish(Status::SkippedPrecheck);
                return Ok(());
            }
            run.save()?;
        }
    }

    let mut info = AgentInfo {
        agent: a.agent().to_string(),
        cwd: repo.display().to_string(),
        repo: repo.display().to_string(),
        ..Default::default()
    };

    // 2. Directory.
    if a.directory() == Directory::Worktree {
        info.base_branch = Some(a.base_branch());
        let wt = create_worktree(a, &repo, &env).context("creating the worktree")?;
        info.cwd = wt.path.display().to_string();
        info.worktree = Some(wt.path.display().to_string());
        info.branch = Some(wt.branch);
        info.base_commit = Some(wt.base_commit);
        run.agent = Some(info.clone());
        run.save()?;
    }

    let prompt = a.prompt_text()?;

    // 3. Space.
    let space_name = a.space_name();
    let (space_id, created) = match a.space_mode() {
        SpaceMode::PerRun => (hc::new_id("sp"), true),
        SpaceMode::Shared => match hc::find_space_by_name(&space_name)? {
            Some(id) => (id, false),
            None => (hc::new_id("sp"), true),
        },
    };
    let tab_id = hc::new_id("tb");
    info.space_id = Some(space_id.clone());
    info.space_name = Some(space_name.clone());
    info.space_created = created;
    info.tab_id = Some(tab_id.clone());

    // 4. Launch, inside a login shell that stays when the agent ends, as the
    //    app starts agents: the space stays readable after a failure.
    let shell = env.get("SHELL").cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let mut argv = vec!["/usr/bin/env".to_string()];
    let mut vars = a.env();
    vars.insert("HERDR_AUTOMATION".into(), a.name.clone());
    vars.insert("HERDR_AUTOMATION_RUN_ID".into(), run.id.clone());
    vars.insert("HERDR_AUTOMATION_TRIGGER".into(), run.trigger.as_str().into());
    argv.extend(vars.iter().map(|(k, v)| format!("{k}={v}")));
    argv.extend([shell.clone(), "-lic".into(), "\"$@\"; exec \"$0\" -li".into(), shell]);
    argv.push(a.agent().to_string());
    if let Some(m) = a.entry.model.clone().filter(|m| !m.is_empty()) {
        argv.push("--model".into());
        argv.push(m);
    }
    argv.extend(a.entry.agent_args.clone().unwrap_or_default());
    argv.push(prompt);

    let terminal = hc::launch(&hc::Launch {
        cwd: &info.cwd,
        labels: vec![
            ("herdr.space".into(), space_id.clone()),
            ("herdr.tab".into(), tab_id.clone()),
            ("automation".into(), a.name.clone()),
            (RUN_LABEL.into(), run.id.clone()),
        ],
        argv,
    })?;
    info.terminal = Some(terminal.clone());
    run.agent = Some(info.clone());
    run.save()?;

    if let Err(e) = hc::name_space_and_tab(&space_id, &space_name, &info.cwd, &tab_id, &a.name, &terminal) {
        run.note = Some(format!("could not name the space: {e:#}"));
    }

    // 5. Watch it.
    let result = watch(a, run, &mut info, &terminal);
    run.agent = Some(info.clone());
    result
}

pub fn human_secs(s: u64) -> String {
    match s {
        s if s % 3600 == 0 => format!("{} h", s / 3600),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn capture(run: &Run, info: &mut AgentInfo, terminal: &str) {
    if let Ok(raw) = hc::read(terminal, OUTPUT_LINES) {
        let mut text = String::with_capacity(raw.len());
        let mut blank = 0;
        for line in raw.lines() {
            let line = line.trim_end();
            blank = if line.is_empty() { blank + 1 } else { 0 };
            if blank <= 1 {
                text.push_str(line);
                text.push('\n');
            }
        }
        let path = run.dir().join("agent.log");
        if std::fs::write(&path, text).is_ok() {
            info.output_file = Some(path.display().to_string());
        }
    }
}

fn add_note(run: &mut Run, note: &str) {
    run.note = Some(match run.note.take() {
        Some(n) => format!("{n}; {note}"),
        None => note.to_string(),
    });
}

fn watch(a: &Automation, run: &mut Run, info: &mut AgentInfo, terminal: &str) -> Result<()> {
    let program = basename(a.agent()).to_string();
    let started = Instant::now();
    let timeout = Duration::from_secs(a.timeout_seconds());
    let mut seen_alive = false;
    let mut done_since: Option<Instant> = None;
    let mut last_capture = Instant::now();
    let mut errors = 0;
    let mut trust_answered = false;

    loop {
        std::thread::sleep(POLL);
        let t = match hc::get(terminal) {
            Ok(t) => {
                errors = 0;
                t
            }
            Err(e) => {
                errors += 1;
                if errors >= 40 {
                    bail!("lost touch with herdr-core: {e:#}");
                }
                continue;
            }
        };

        let mut live = t.as_ref().map(|t| t["state"] == "live").unwrap_or(false);
        let agent_gone = |t: &Option<Value>| {
            t.as_ref()
                .and_then(|t| t["foreground"]["argv"][0].as_str().map(|a| basename(a) != program))
                .unwrap_or(true)
        };
        if live && seen_alive && agent_gone(&t) {
            // Closing a space ends the agent a moment before its terminal:
            // look again before calling it an exit.
            std::thread::sleep(Duration::from_secs(3));
            live = hc::get(terminal).ok().flatten().map(|t| t["state"] == "live").unwrap_or(live);
        }
        if !live {
            // The terminal ended without us: its space or tab was closed by hand.
            capture(run, info, terminal);
            info.closed = Some("closed by hand".into());
            add_note(run, "its space or tab was closed by hand");
            cleanup_after_close(info);
            run.finish(Status::Closed);
            return Ok(());
        }
        let t: Value = t.unwrap();
        let status = t["status"]["state"].as_str().unwrap_or("");
        let turn = t["turn"].as_u64().unwrap_or(0);
        let fg = t["foreground"]["argv"][0].as_str().map(basename).unwrap_or("");
        let alive = fg == program;
        seen_alive |= alive;

        if !info.prompt_accepted && (turn >= 1 || status == "working") {
            info.prompt_accepted = true;
            run.agent = Some(info.clone());
            run.save()?;
        }

        // Waiting on the user: hand the run to them. It stays open, with no
        // timeout, until the agent is done or the space is closed.
        if status == "blocked" || status == "error" {
            done_since = None;
            if run.status != Status::Blocked {
                run.status = Status::Blocked;
                info.was_blocked = true;
                capture(run, info, terminal);
                run.agent = Some(info.clone());
                run.save()?;
            }
            continue;
        } else if run.status == Status::Blocked {
            run.status = Status::Running;
            run.save()?;
        }

        if !info.prompt_accepted {
            if !trust_answered {
                trust_answered = answer_trust_question(run, info, terminal);
            }
            let gave_up = (seen_alive && !alive) || (started.elapsed() > START_TIMEOUT && !info.was_blocked);
            if gave_up {
                capture(run, info, terminal);
                add_note(
                    run,
                    if seen_alive && !alive { "the agent exited before taking the prompt" } else { "the agent never took the prompt" },
                );
                close_ours(info, &run.id);
                run.finish(Status::Failed);
                return Ok(());
            }
            continue;
        }

        if seen_alive && !alive {
            capture(run, info, terminal);
            add_note(run, "the agent exited before reporting done; its space stays open");
            run.finish(Status::Failed);
            return Ok(());
        }

        if (status == "done" || status == "idle") && alive {
            let since = *done_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= SETTLE {
                capture(run, info, terminal);
                if info.was_blocked {
                    add_note(run, "you stepped in while it was blocked, so its space stays open");
                } else {
                    close_ours(info, &run.id);
                }
                run.finish(Status::Succeeded);
                return Ok(());
            }
        } else {
            done_since = None;
        }

        if !info.was_blocked && started.elapsed() > timeout {
            stop_agent(&t, terminal, &program);
            capture(run, info, terminal);
            run.timed_out = true;
            add_note(run, &format!("stopped after the {} timeout; its space stays open", human_secs(a.timeout_seconds())));
            run.finish(Status::Failed);
            return Ok(());
        }

        if last_capture.elapsed() >= CAPTURE_EVERY {
            capture(run, info, terminal);
            run.agent = Some(info.clone());
            run.save()?;
            last_capture = Instant::now();
        }
    }
}

/// Claude asks whether to trust each new folder, and a worktree is always
/// new. For the worktree this run made from the configured repo (and no
/// other folder, and no other question), answer yes. Returns whether it did.
fn answer_trust_question(run: &mut Run, info: &AgentInfo, terminal: &str) -> bool {
    let Some(worktree) = info.worktree.as_deref() else { return false };
    let Ok(screen) = hc::read(terminal, 0) else { return false };
    let asking = screen.contains("Yes, I trust this folder") && screen.contains(worktree);
    if !asking {
        return false;
    }
    // The options are "No, exit" (selected) then "Yes, I trust this folder".
    if hc::keys(terminal, &["down", "enter"]).is_ok() {
        add_note(run, "answered Claude's folder-trust question for its own worktree");
        return true;
    }
    false
}

/// Stops the agent but not its shell, so the space keeps what it showed.
fn stop_agent(t: &Value, terminal: &str, program: &str) {
    let Some(pid) = t["foreground"]["pid"].as_i64() else { return };
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    for _ in 0..10 {
        std::thread::sleep(Duration::from_secs(1));
        let still = hc::get(terminal)
            .ok()
            .flatten()
            .map(|t| t["foreground"]["pid"].as_i64() == Some(pid) && t["foreground"]["argv"][0].as_str().map(basename) == Some(program))
            .unwrap_or(false);
        if !still {
            return;
        }
    }
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

fn addresses(list: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = list.iter().filter_map(|t| t["address"].as_str().map(String::from)).collect();
    v.sort();
    v
}

/// Closes what this run opened and nothing else: its space if it made the
/// space and every terminal there is this run's; else only its tab, if every
/// terminal in the tab is this run's.
fn close_ours(info: &mut AgentInfo, run_id: &str) {
    let (Some(space), Some(tab)) = (info.space_id.clone(), info.tab_id.clone()) else { return };
    let ours = |label: &str, id: &str| -> Option<Vec<String>> {
        let all = hc::live_with_labels(&[(label, id)]).ok()?;
        let mine = hc::live_with_labels(&[(label, id), (RUN_LABEL, run_id)]).ok()?;
        let (all, mine) = (addresses(&all), addresses(&mine));
        (all == mine).then_some(mine)
    };
    let (targets, what) = if info.space_created {
        match ours("herdr.space", &space) {
            Some(t) => (Some(t), "space closed"),
            None => (ours("herdr.tab", &tab), "tab closed (the space has other work in it)"),
        }
    } else {
        (ours("herdr.tab", &tab), "tab closed")
    };
    let Some(targets) = targets else {
        info.closed = Some("left open: it holds terminals this run didn't start".into());
        return;
    };
    for t in &targets {
        let _ = hc::kill(t);
    }
    // The app drops ended terminals' tabs itself; when it isn't running, we do.
    std::thread::sleep(Duration::from_secs(2));
    let _ = hc::forget_tab(&space, &tab);
    info.closed = Some(what.into());
    cleanup_after_close(info);
}

fn cleanup_after_close(info: &mut AgentInfo) {
    if info.worktree.is_some() && info.worktree_cleanup.is_none() {
        info.worktree_cleanup = Some(cleanup_worktree(info));
    }
}

struct Worktree {
    path: PathBuf,
    branch: String,
    base_commit: String,
}

fn git(repo: &Path, args: &[&str], timeout: u64) -> Result<String> {
    let env = crate::env::login_env();
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo).args(args).env_clear().envs(&env).stdin(std::process::Stdio::null());
    let out = proc::output_with_timeout(cmd, Duration::from_secs(timeout))?;
    if out.timed_out {
        bail!("git {} timed out", args.join(" "));
    }
    if out.exit_code != Some(0) {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn create_worktree(a: &Automation, repo: &Path, _env: &std::collections::BTreeMap<String, String>) -> Result<Worktree> {
    let base = a.base_branch();
    if let Some((remote, branch)) = base.split_once('/') {
        let remotes = git(repo, &["remote"], 30).unwrap_or_default();
        if remotes.lines().any(|r| r == remote) {
            git(repo, &["fetch", remote, branch], 180).context("fetching the base branch")?;
        }
    }
    let base_commit = git(repo, &["rev-parse", "--verify", &format!("{base}^{{commit}}")], 30)?;
    let branch = format!("automation/{}/{}", a.name, Utc::now().format("%Y%m%d-%H%M%S"));
    let repo_name = repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "repo".into());
    let path = paths::home().join(".herdr/worktrees").join(repo_name).join(&branch);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    git(repo, &["worktree", "add", "-b", &branch, &path.display().to_string(), &base_commit], 300)?;
    Ok(Worktree { path, branch, base_commit })
}

/// `git worktree remove` without force; the branch goes only if it has no
/// commits beyond its base, or they're merged or pushed.
pub fn cleanup_worktree(info: &AgentInfo) -> String {
    let repo = PathBuf::from(&info.repo);
    let Some(path) = info.worktree.clone() else { return "no worktree".into() };
    if Path::new(&path).exists() {
        if let Err(e) = git(&repo, &["worktree", "remove", &path], 120) {
            return format!("kept worktree {path}: {e:#}");
        }
    }
    // Branch names with slashes leave empty folders behind.
    let root = paths::home().join(".herdr/worktrees");
    let mut dir = Path::new(&path).parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        if !d.starts_with(&root) || d == root || std::fs::remove_dir(&d).is_err() {
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    let Some(branch) = info.branch.clone() else { return "removed worktree".into() };
    if git(&repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")], 30).is_err() {
        return "removed worktree (branch already gone)".into();
    }
    let ahead = info
        .base_commit
        .as_ref()
        .and_then(|b| git(&repo, &["rev-list", "--count", &format!("{b}..{branch}")], 30).ok())
        .and_then(|n| n.parse::<u64>().ok());
    let reason = match ahead {
        Some(0) => Some("no new commits"),
        _ => {
            let base_ref = info.base_branch.clone().unwrap_or_else(|| "origin/main".into());
            let merged = git(&repo, &["merge-base", "--is-ancestor", &branch, &base_ref], 30).is_ok();
            let pushed = git(&repo, &["branch", "-r", "--contains", &branch], 30)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if merged {
                Some("merged")
            } else if pushed {
                Some("pushed")
            } else {
                None
            }
        }
    };
    match reason {
        Some(why) => match git(&repo, &["branch", "-D", &branch], 30) {
            Ok(_) => format!("removed worktree and branch ({why})"),
            Err(e) => format!("removed worktree; kept branch {branch}: {e:#}"),
        },
        None => format!(
            "removed worktree; kept branch {branch}: {} commit(s) not merged or pushed",
            ahead.map(|n| n.to_string()).unwrap_or_else(|| "some".into())
        ),
    }
}

/// For the scheduler: removes the worktree of every ended run whose space is
/// gone now (closed by hand after a failure, say).
pub fn reap_worktrees() {
    for mut r in history::all() {
        if r.status.is_live() {
            continue;
        }
        let Some(info) = r.agent.clone() else { continue };
        if info.worktree.is_none() || info.worktree_cleanup.is_some() {
            continue;
        }
        let Ok(live) = hc::live_with_labels(&[(RUN_LABEL, &r.id)]) else { continue };
        if !live.is_empty() {
            continue;
        }
        let mut info = info;
        info.worktree_cleanup = Some(cleanup_worktree(&info));
        r.agent = Some(info);
        let _ = r.save();
    }
}
