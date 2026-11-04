//! Tabs that a run opens for its own helpers. `tab open` starts a terminal as
//! a new tab in the caller's space (an agent run's space, say), and `tab
//! close` ends tabs opened that way. A run can fan work out into several tabs
//! of its space and close each one when it's finished.
//!
//! Every tab opened here carries `automation.opened-by`, and `tab close` only
//! ever ends terminals carrying it. It never touches the run's own tab, which
//! the runner closes, or anything the user opened.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::{env, hc};

pub const OPENED_BY: &str = "automation.opened-by";
const START_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(clap::Subcommand)]
pub enum Action {
    /// Open a terminal as a new tab in the space this command runs in
    Open {
        /// The tab's name
        #[arg(long)]
        name: String,
        /// Where it starts
        #[arg(long)]
        cwd: String,
        /// A label to add, key=value (repeatable)
        #[arg(long = "label")]
        labels: Vec<String>,
        /// The space's id (default: the space of the terminal running this)
        #[arg(long)]
        space: Option<String>,
        /// Answer Claude's folder-trust question, for --cwd and no other folder
        #[arg(long)]
        trust: bool,
        /// Wait until the agent has taken its prompt (up to 3 minutes)
        #[arg(long)]
        wait_start: bool,
        /// The program and its arguments, after `--` (default: a login shell).
        /// It runs in a login shell that stays when it ends.
        #[arg(last = true)]
        argv: Vec<String>,
    },
    /// End tabs opened with `tab open` that carry every label given
    Close {
        /// key=value (repeatable)
        #[arg(long = "label")]
        labels: Vec<String>,
        /// End this terminal's own tab (it must have been opened with `tab open`)
        #[arg(long = "self")]
        this: bool,
        /// With --label: leave this terminal running
        #[arg(long)]
        except_self: bool,
    },
}

pub fn run(action: Action) -> Result<()> {
    match action {
        Action::Open { name, cwd, labels, space, trust, wait_start, argv } => {
            let out = open(&name, &cwd, &labels, space, trust, wait_start, argv)?;
            println!("{out}");
            Ok(())
        }
        Action::Close { labels, this, except_self } => close(&labels, this, except_self),
    }
}

fn parse_labels(raw: &[String]) -> Result<Vec<(String, String)>> {
    raw.iter()
        .map(|l| {
            l.split_once('=')
                .filter(|(k, _)| !k.is_empty())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| anyhow!("label `{l}` is not key=value"))
        })
        .collect()
}

/// The terminal running this command, from herdr-core.
fn caller() -> Result<Option<Value>> {
    match std::env::var("HC_TERMINAL_ID").ok().filter(|s| !s.is_empty()) {
        Some(addr) => hc::get(&addr),
        None => Ok(None),
    }
}

fn label(t: &Value, key: &str) -> Option<String> {
    t["labels"][key].as_str().map(String::from)
}

fn open(
    name: &str,
    cwd: &str,
    raw_labels: &[String],
    space: Option<String>,
    trust: bool,
    wait_start: bool,
    argv: Vec<String>,
) -> Result<Value> {
    let cwd = shellexpand_home(cwd);
    if !std::path::Path::new(&cwd).is_dir() {
        bail!("--cwd {cwd} is not a folder");
    }
    let me = caller()?;
    let space = match space.or_else(|| me.as_ref().and_then(|t| label(t, "herdr.space"))) {
        Some(s) => s,
        None => bail!("no space: run this from a terminal in a Herdr space, or pass --space"),
    };
    // Who opened it: the run (its id), or whoever opened the caller.
    let opened_by = me
        .as_ref()
        .and_then(|t| label(t, crate::agent::RUN_LABEL).or_else(|| label(t, OPENED_BY)))
        .or_else(|| std::env::var("HERDR_AUTOMATION_RUN_ID").ok())
        .unwrap_or_else(|| "manual".into());
    let automation = me.as_ref().and_then(|t| label(t, "automation")).or_else(|| std::env::var("HERDR_AUTOMATION").ok());

    let tab = hc::new_id("tb");
    let mut labels = vec![
        ("herdr.space".to_string(), space.clone()),
        ("herdr.tab".to_string(), tab.clone()),
        (OPENED_BY.to_string(), opened_by),
    ];
    if let Some(a) = automation {
        labels.push(("automation".into(), a));
    }
    labels.extend(parse_labels(raw_labels)?);

    let login = env::login_env();
    let shell = login.get("SHELL").cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let program = argv.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_string());
    let full = if argv.is_empty() {
        vec![shell.clone(), "-li".into()]
    } else {
        let mut v = vec![shell.clone(), "-lic".into(), "\"$@\"; exec \"$0\" -li".into(), shell];
        v.extend(argv);
        v
    };
    let terminal = hc::launch(&hc::Launch { cwd: &cwd, labels, argv: full })?;
    let named = hc::name_tab(&space, &tab, name, &terminal).err().map(|e| format!("{e:#}"));

    let mut out = json!({ "terminal": terminal, "space": space, "tab": tab, "cwd": cwd });
    if let Some(e) = named {
        out["note"] = json!(format!("could not name the tab: {e}"));
    }
    if trust || wait_start {
        let (took, trusted) = wait_for_start(&terminal, &cwd, program.as_deref(), trust, wait_start)?;
        out["trust_answered"] = json!(trusted);
        if wait_start {
            out["prompt_accepted"] = json!(took);
            if !took {
                bail!("{terminal} never took its prompt (screen: {})", out_tail(&terminal));
            }
        }
    }
    Ok(out)
}

fn shellexpand_home(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => crate::paths::home().join(rest).display().to_string(),
        None => p.to_string(),
    }
}

fn out_tail(terminal: &str) -> String {
    hc::read(terminal, 0)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" / "))
        .unwrap_or_default()
}

/// Answers the folder-trust question for `cwd` when asked to, and with
/// `wait_start` waits for the agent to take its prompt. Returns (took, trusted).
fn wait_for_start(terminal: &str, cwd: &str, program: Option<&str>, trust: bool, wait_start: bool) -> Result<(bool, bool)> {
    let started = Instant::now();
    let mut trusted = false;
    let mut seen_alive = false;
    while started.elapsed() < START_TIMEOUT {
        std::thread::sleep(Duration::from_secs(2));
        let Some(t) = hc::get(terminal)? else { bail!("{terminal} ended") };
        if t["state"] != "live" {
            bail!("{terminal} ended");
        }
        let turn = t["turn"].as_u64().unwrap_or(0);
        let status = t["status"]["state"].as_str().unwrap_or("");
        if turn >= 1 || status == "working" {
            return Ok((true, trusted));
        }
        let fg = t["foreground"]["argv"][0].as_str().map(|a| a.rsplit('/').next().unwrap_or(a).to_string());
        let alive = program.is_some() && fg.as_deref() == program;
        if seen_alive && !alive {
            bail!("the program exited before taking its prompt (screen: {})", out_tail(terminal));
        }
        seen_alive |= alive;
        if trust && !trusted {
            let screen = hc::read(terminal, 0).unwrap_or_default();
            // The options are "No, exit" (selected) then "Yes, I trust this folder".
            if screen.contains("Yes, I trust this folder") && screen.contains(cwd) {
                hc::keys(terminal, &["down", "enter"]).context("answering the folder-trust question")?;
                trusted = true;
                if !wait_start {
                    return Ok((false, true));
                }
            }
        }
        // Nothing to wait for once the question can no longer come.
        if !wait_start && turn == 0 && started.elapsed() > Duration::from_secs(30) {
            return Ok((false, trusted));
        }
    }
    Ok((false, trusted))
}

fn close(raw_labels: &[String], this: bool, except_self: bool) -> Result<()> {
    let me = std::env::var("HC_TERMINAL_ID").ok().filter(|s| !s.is_empty());
    let targets: Vec<Value> = if this {
        let Some(t) = caller()? else { bail!("--self: this isn't running in a herdr-core terminal") };
        if label(&t, OPENED_BY).is_none() {
            bail!("--self: this terminal wasn't opened with `tab open`, so it isn't closed here");
        }
        vec![t]
    } else {
        let labels = parse_labels(raw_labels)?;
        if labels.is_empty() {
            bail!("give --label (or --self)");
        }
        let refs: Vec<(&str, &str)> = labels.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        // `hc ls` leaves labels out; read each terminal in full.
        hc::live_with_labels(&refs)?
            .into_iter()
            .filter_map(|t| t["address"].as_str().and_then(|a| hc::get(a).ok().flatten()))
            .filter(|t| label(t, OPENED_BY).is_some())
            .filter(|t| !(except_self && t["address"].as_str() == me.as_deref()))
            .collect()
    };
    let mut closed = Vec::new();
    // Our own terminal last: nothing runs after it.
    let (mine, others): (Vec<_>, Vec<_>) = targets.into_iter().partition(|t| t["address"].as_str() == me.as_deref());
    for t in others.iter().chain(mine.iter()) {
        let Some(addr) = t["address"].as_str() else { continue };
        if t["address"].as_str() == me.as_deref() {
            println!("closing {addr} (this terminal)");
        }
        hc::kill(addr)?;
        closed.push(addr.to_string());
        forget_if_empty(t);
    }
    if mine.is_empty() {
        println!("closed {}", if closed.is_empty() { "nothing".into() } else { closed.join(", ") });
    }
    Ok(())
}

/// The app drops a tab whose terminals ended; when it isn't running, we do.
fn forget_if_empty(t: &Value) {
    let (Some(space), Some(tab)) = (label(t, "herdr.space"), label(t, "herdr.tab")) else { return };
    std::thread::sleep(Duration::from_millis(500));
    let Ok(left) = hc::live_with_labels(&[("herdr.tab", &tab)]) else { return };
    if left.is_empty() {
        let _ = hc::forget_tab(&space, &tab);
    }
}
