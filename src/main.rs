mod agent;
mod config;
mod cron;
mod env;
mod hc;
mod history;
mod lock;
mod paths;
mod proc;
mod runner;
mod scheduler;
mod service;
mod tabs;

use std::io::IsTerminal;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local, Utc};
use clap::{Parser, Subcommand};
use serde_json::json;

use config::Slot;
use history::{Run, Status, Trigger};

#[derive(Parser)]
#[command(name = "automate-everything", version, about = "Scheduled scripts and agent runs for the Herdr app")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Every automation: status, last run, next run
    List {
        #[arg(long)]
        json: bool,
    },
    /// Run one now (skips the precheck; works on a paused entry)
    Run {
        name: String,
        /// Stay until it finishes and print the result
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        json: bool,
    },
    /// Stop scheduling it, keeping the entry
    Pause { name: String },
    /// Schedule it again
    Resume { name: String },
    /// Remove the entry from the config and delete its history
    Delete {
        name: String,
        #[arg(long)]
        yes: bool,
    },
    /// Runs and skips, newest first
    History {
        name: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// One run: its record, precheck result and output
    Show {
        run_id: String,
        #[arg(long)]
        json: bool,
    },
    /// One entry's fields as JSON
    Get { name: String },
    /// Create or change one entry from JSON fields (null removes a field)
    Set {
        name: String,
        /// JSON object of fields
        #[arg(long)]
        fields: String,
        /// The entry must not exist yet
        #[arg(long)]
        create: bool,
    },
    /// Check the config and report invalid entries
    Check {
        #[arg(long)]
        json: bool,
    },
    /// Whether the scheduler is running, and where things live
    Status {
        #[arg(long)]
        json: bool,
    },
    /// The launchd agent that keeps the scheduler running
    Service {
        #[command(subcommand)]
        action: service::Action,
    },
    /// Tabs a run opens in its own space for helper work, and closes again
    Tab {
        #[command(subcommand)]
        action: tabs::Action,
    },
    /// Run the scheduler in the foreground (launchd does this)
    Scheduler,
    #[command(name = "_run", hide = true)]
    RunInternal {
        name: String,
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        trigger: String,
        #[arg(long)]
        scheduled_for: Option<String>,
        #[arg(long)]
        caught_up: Option<u32>,
    },
}

fn local(t: DateTime<Utc>) -> String {
    let l = t.with_timezone(&Local);
    if l.date_naive() == Local::now().date_naive() {
        l.format("today %H:%M").to_string()
    } else {
        l.format("%a %d %b %H:%M").to_string()
    }
}

fn ago(t: DateTime<Utc>) -> String {
    let s = (Utc::now() - t).num_seconds();
    if s < 0 {
        let s = -s;
        return match s {
            0..=59 => format!("in {s}s"),
            60..=3599 => format!("in {}m", s / 60),
            3600..=86399 => format!("in {}h", s / 3600),
            _ => format!("in {}d", s / 86400),
        };
    }
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn run_time(r: &Run) -> DateTime<Utc> {
    r.started_at.or(r.scheduled_for).unwrap_or(r.created_at)
}

fn list(json_out: bool) -> Result<()> {
    let cfg = config::load()?;
    let runs = history::all();
    // Last run: the newest record that has ended (a live run shows as `running`).
    let ended: Vec<Run> = runs.iter().filter(|r| !r.status.is_live()).cloned().collect();
    let latest = history::latest_by_automation(&ended);
    let now = Utc::now();
    let mut items = Vec::new();
    for slot in &cfg.slots {
        let name = slot.name().to_string();
        let running = lock::is_held(&paths::automation_lock(&name));
        let last = latest.get(&name);
        let live = runs.iter().find(|r| r.automation == name && r.status.is_live());
        let item = match slot {
            Slot::Valid(a) => {
                let status = if running {
                    "running"
                } else if a.paused() {
                    "paused"
                } else {
                    "active"
                };
                let next = if a.paused() { None } else { a.cron.next_after(&a.tz, now) };
                json!({
                    "name": name, "kind": a.kind.as_str(), "status": status, "paused": a.paused(),
                    "schedule": a.entry.schedule, "timezone": a.tz.name(), "description": a.entry.description,
                    "grace_minutes": a.grace_minutes(), "error": null,
                    "next": next, "last": last, "live": live,
                })
            }
            Slot::Invalid(i) => json!({
                "name": name, "kind": i.kind, "status": if running { "running" } else { "invalid" }, "paused": i.paused,
                "schedule": i.schedule, "timezone": null, "description": null, "error": i.error,
                "next": null, "last": last, "live": live,
            }),
        };
        items.push(item);
    }
    if json_out {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if items.is_empty() {
        println!("No automations in {}", paths::config_file().display());
        return Ok(());
    }
    let w = items.iter().map(|i| i["name"].as_str().unwrap_or("").len()).max().unwrap_or(4).max(4);
    println!("{:w$}  {:6}  {:8}  {:34}  {}", "NAME", "KIND", "STATUS", "LAST RUN", "NEXT RUN");
    for i in &items {
        let last = match i["last"].as_object() {
            Some(_) => {
                let r: Run = serde_json::from_value(i["last"].clone())?;
                format!("{} · {}", r.status.as_str(), local(run_time(&r)))
            }
            None => "never".into(),
        };
        let next = match i["next"].as_str() {
            Some(s) => {
                let t = DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc);
                format!("{} ({})", local(t), ago(t))
            }
            None => "-".into(),
        };
        println!(
            "{:w$}  {:6}  {:8}  {:34}  {}",
            i["name"].as_str().unwrap_or(""),
            i["kind"].as_str().unwrap_or("?"),
            i["status"].as_str().unwrap_or(""),
            last,
            next
        );
        if let Some(e) = i["error"].as_str() {
            println!("{:w$}  ! {e}", "");
        }
    }
    Ok(())
}

fn run_now(name: &str, wait: bool, json_out: bool) -> Result<()> {
    let cfg = config::load()?;
    match cfg.find(name) {
        None => bail!("no automation named `{name}`"),
        Some(Slot::Invalid(i)) => bail!("`{name}` is invalid: {}", i.error),
        Some(Slot::Valid(_)) => {}
    }
    let req = runner::Request {
        name: name.to_string(),
        trigger: Trigger::Manual,
        run_id: history::new_id(name),
        scheduled_for: None,
        caught_up: None,
    };
    if wait {
        let run = runner::execute(req)?;
        if json_out {
            println!("{}", serde_json::to_string_pretty(&run)?);
        } else {
            print_run(&run, true)?;
        }
        if run.status != Status::Succeeded {
            std::process::exit(1);
        }
        return Ok(());
    }
    if lock::is_held(&paths::automation_lock(name)) && !json_out {
        eprintln!("`{name}` is still running; this run will be recorded as skipped_overlap");
    }
    paths::ensure_state_dirs()?;
    let mut child = runner::spawn(&req)?;
    // Let it get going, so its record exists by the time we return.
    std::thread::sleep(std::time::Duration::from_millis(150));
    let _ = child.try_wait();
    if json_out {
        println!("{}", json!({ "run_id": req.run_id }));
    } else {
        println!("started {} ({})", name, req.run_id);
    }
    Ok(())
}

fn tail_file(path: &std::path::Path, max: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(proc::tail_string(&bytes, max))
}

fn output_of(r: &Run) -> serde_json::Value {
    let dir = r.dir();
    let agent_out = r
        .agent
        .as_ref()
        .and_then(|a| a.output_file.as_ref())
        .and_then(|f| tail_file(std::path::Path::new(f), 256 * 1024));
    json!({
        "stdout": tail_file(&dir.join("stdout.log"), 256 * 1024),
        "stderr": tail_file(&dir.join("stderr.log"), 256 * 1024),
        "agent": agent_out,
    })
}

fn print_run(r: &Run, with_output: bool) -> Result<()> {
    println!("{}  {}  {}  {}", r.id, r.automation, r.trigger.as_str(), r.status.as_str());
    if let Some(t) = r.scheduled_for {
        println!("  scheduled for  {}", local(t));
    }
    if let Some(t) = r.started_at {
        println!("  started        {}", local(t));
    }
    if let Some(t) = r.finished_at {
        println!("  finished       {}", local(t));
    }
    if let Some(ms) = r.duration_ms {
        println!("  duration       {:.1}s", ms as f64 / 1000.0);
    }
    if r.exit_code.is_some() || r.signal.is_some() {
        let code = r.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "none".into());
        println!("  exit           {code}{}", r.signal.map(|s| format!(" (signal {s})")).unwrap_or_default());
    }
    if r.timed_out {
        println!("  timed out      yes");
    }
    if let Some(n) = r.caught_up {
        println!("  caught up      {n} earlier fire(s)");
    }
    if let Some(n) = &r.note {
        println!("  note           {n}");
    }
    if let Some(a) = &r.agent {
        println!("  agent          {} in {}", a.agent, a.cwd);
        if let Some(s) = &a.space_name {
            println!("  space          {s}{}", a.space_id.as_ref().map(|i| format!(" ({i})")).unwrap_or_default());
        }
        if let Some(t) = &a.terminal {
            println!("  terminal       {t}");
        }
        if let Some(w) = &a.worktree {
            println!("  worktree       {w}");
        }
        if let Some(c) = &a.closed {
            println!("  closed         {c}");
        }
        if let Some(c) = &a.worktree_cleanup {
            println!("  worktree       {c}");
        }
    }
    if let Some(p) = &r.precheck {
        println!(
            "  precheck       exit {}{} in {:.1}s: {}",
            p.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "none".into()),
            if p.timed_out { ", timed out" } else { "" },
            p.duration_ms as f64 / 1000.0,
            p.command
        );
        if !p.stdout.trim().is_empty() {
            println!("--- precheck stdout\n{}", p.stdout.trim_end());
        }
        if !p.stderr.trim().is_empty() {
            println!("--- precheck stderr\n{}", p.stderr.trim_end());
        }
    }
    if with_output {
        let out = output_of(r);
        for (k, label) in [("stdout", "stdout"), ("stderr", "stderr"), ("agent", "agent output")] {
            if let Some(s) = out[k].as_str() {
                if !s.trim().is_empty() {
                    println!("--- {label}\n{}", s.trim_end());
                }
            }
        }
    }
    Ok(())
}

fn history_cmd(name: Option<String>, limit: usize, json_out: bool) -> Result<()> {
    let runs: Vec<Run> = history::all()
        .into_iter()
        .filter(|r| name.as_ref().map(|n| &r.automation == n).unwrap_or(true))
        .take(limit)
        .collect();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&runs)?);
        return Ok(());
    }
    for r in &runs {
        let dur = r.duration_ms.map(|ms| format!("{:.0}s", ms as f64 / 1000.0)).unwrap_or_default();
        println!(
            "{:16}  {:26}  {:9}  {:16}  {:>6}  {}",
            local(run_time(r)),
            r.automation,
            r.trigger.as_str(),
            r.status.as_str(),
            dur,
            r.id
        );
    }
    Ok(())
}

fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{question} [y/N] ");
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).ok();
    matches!(s.trim(), "y" | "Y" | "yes")
}

fn get_entry(name: &str) -> Result<()> {
    let table: toml::Table = toml::from_str(&config::read_text()?)?;
    let entry = table
        .get("automations")
        .and_then(|a| a.get(name))
        .ok_or_else(|| anyhow!("no automation named `{name}`"))?;
    println!("{}", serde_json::to_string_pretty(entry)?);
    Ok(())
}

fn check(json_out: bool) -> Result<()> {
    let cfg = config::load()?;
    let invalid: Vec<_> = cfg
        .slots
        .iter()
        .filter_map(|s| match s {
            Slot::Invalid(i) => Some(json!({ "name": i.name, "error": i.error })),
            _ => None,
        })
        .collect();
    if json_out {
        println!("{}", json!({ "ok": invalid.is_empty(), "count": cfg.slots.len(), "invalid": invalid }));
    } else if invalid.is_empty() {
        println!("{} automations, all valid", cfg.slots.len());
    } else {
        for i in &invalid {
            println!("{}: {}", i["name"].as_str().unwrap_or(""), i["error"].as_str().unwrap_or(""));
        }
    }
    if !invalid.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

fn status(json_out: bool) -> Result<()> {
    let running = lock::is_held(&paths::scheduler_lock());
    let note = lock::read_note(&paths::scheduler_lock());
    let v = json!({
        "scheduler_running": running,
        "scheduler": if running { note } else { None },
        "service_installed": service::plist_path().exists(),
        "config": paths::config_file(),
        "state": paths::state_dir(),
        "log": paths::scheduler_log(),
    });
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!(
            "scheduler  {}",
            if running { format!("running ({})", v["scheduler"].as_str().unwrap_or("")) } else { "not running".into() }
        );
        println!("service    {}", if v["service_installed"].as_bool() == Some(true) { "installed" } else { "not installed" });
        println!("config     {}", paths::config_file().display());
        println!("state      {}", paths::state_dir().display());
        println!("log        {}", paths::scheduler_log().display());
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List { json } => list(json),
        Cmd::Run { name, wait, json } => run_now(&name, wait, json),
        Cmd::Pause { name } => {
            config::set_paused(&name, true)?;
            println!("paused {name}");
            Ok(())
        }
        Cmd::Resume { name } => {
            config::set_paused(&name, false)?;
            println!("resumed {name}");
            Ok(())
        }
        Cmd::Delete { name, yes } => {
            if !yes && !confirm(&format!("Delete `{name}` and its run history?")) {
                bail!("not deleted (pass --yes to skip the question)");
            }
            config::remove(&name)?;
            let n = history::delete_for(&name);
            println!("deleted {name} and {n} history records");
            Ok(())
        }
        Cmd::History { name, limit, json } => history_cmd(name, limit, json),
        Cmd::Show { run_id, json } => {
            let r = history::load(&run_id)?;
            if json {
                let mut v = serde_json::to_value(&r)?;
                v["output"] = output_of(&r);
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                print_run(&r, true)?;
            }
            Ok(())
        }
        Cmd::Get { name } => get_entry(&name),
        Cmd::Set { name, fields, create } => {
            let v: serde_json::Value = serde_json::from_str(&fields).context("--fields must be a JSON object")?;
            let obj = v.as_object().ok_or_else(|| anyhow!("--fields must be a JSON object"))?;
            config::upsert(&name, obj, create)?;
            println!("saved {name}");
            Ok(())
        }
        Cmd::Check { json } => check(json),
        Cmd::Status { json } => status(json),
        Cmd::Service { action } => service::run(action),
        Cmd::Tab { action } => tabs::run(action),
        Cmd::Scheduler => scheduler::serve(),
        Cmd::RunInternal { name, run_id, trigger, scheduled_for, caught_up } => {
            let trigger = match trigger.as_str() {
                "scheduled" => Trigger::Scheduled,
                "manual" => Trigger::Manual,
                t => bail!("unknown trigger `{t}`"),
            };
            let scheduled_for = scheduled_for
                .map(|s| DateTime::parse_from_rfc3339(&s).map(|t| t.with_timezone(&Utc)))
                .transpose()?;
            let run = runner::execute(runner::Request { name, trigger, run_id, scheduled_for, caught_up })?;
            eprintln!("{} {} {}", Utc::now().format("%Y-%m-%dT%H:%M:%SZ"), run.id, run.status.as_str());
            Ok(())
        }
    }
}
