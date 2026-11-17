mod agent;
mod config;
mod cron;
mod env;
mod hc;
mod host;
mod history;
mod lock;
mod paths;
mod proc;
mod remote;
mod runner;
mod scheduler;
mod service;
mod tabs;
mod transfer;

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
    /// Ask automate-everything on another herdr-core machine (`hc machine
    /// ls`) instead: its own config, history and scheduler
    #[arg(long, global = true, value_name = "MACHINE")]
    on: Option<String>,
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
        /// The run's id (for `--on`, which picks it before starting the run)
        #[arg(long, hide = true)]
        run_id: Option<String>,
        /// Only check it could run, start nothing (for `--on`)
        #[arg(long, hide = true)]
        check: bool,
    },
    /// Stop scheduling it, keeping the entry
    Pause { name: String },
    /// Schedule it again
    Resume { name: String },
    /// Move the entry to another place in the list
    Move {
        name: String,
        /// Its new position, 1 being the top
        #[arg(long)]
        to: usize,
    },
    /// Remove the entry from the config and delete its history
    Delete {
        name: String,
        #[arg(long)]
        yes: bool,
        /// Remove only the entry, keeping its run history
        #[arg(long)]
        keep_history: bool,
    },
    /// Hand the entry to another scheduler: `--to <machine>` makes that
    /// machine keep the schedule and fire it (even while this one sleeps),
    /// `--to here` brings it back. With `--on`, the entry comes from there.
    /// Its history stays where it was.
    Transfer {
        name: String,
        /// A machine (`hc machine ls`), or `here` for this one
        #[arg(long)]
        to: String,
        /// JSON object of fields to change on the way (as for `set`)
        #[arg(long)]
        fields: Option<String>,
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
    /// The hc machines an automation can run on (`hc machine ls`)
    Machines {
        #[arg(long)]
        json: bool,
        /// Also ask each connected one whether it runs automate-everything
        /// (version, scheduler) so entries can be scheduled by it
        #[arg(long)]
        probe: bool,
    },
    /// Whether the scheduler is running, and where things live
    Status {
        #[arg(long)]
        json: bool,
    },
    /// The background service that keeps the scheduler running (launchd on
    /// macOS, systemd on Linux)
    Service {
        #[command(subcommand)]
        action: service::Action,
    },
    /// Tabs a run opens in its own space for helper work, and closes again
    Tab {
        #[command(subcommand)]
        action: tabs::Action,
    },
    /// Run the scheduler in the foreground (the background service does this)
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
                    "grace_minutes": a.grace_minutes(), "machine": a.machine(), "error": null,
                    "next": next, "last": last, "live": live,
                })
            }
            Slot::Invalid(i) => json!({
                "name": name, "kind": i.kind, "status": if running { "running" } else { "invalid" }, "paused": i.paused,
                "schedule": i.schedule, "timezone": null, "description": null, "machine": i.machine, "error": i.error,
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
    // The MACHINE column only shows once some entry runs elsewhere.
    let mw = items.iter().filter_map(|i| i["machine"].as_str()).map(str::len).max().map(|n| n.max(7));
    let on = |i: &serde_json::Value| match mw {
        Some(mw) => format!("{:mw$}  ", i["machine"].as_str().unwrap_or("-")),
        None => String::new(),
    };
    let header = mw.map(|mw| format!("{:mw$}  ", "MACHINE")).unwrap_or_default();
    println!("{:w$}  {:6}  {header}{:8}  {:34}  {}", "NAME", "KIND", "STATUS", "LAST RUN", "NEXT RUN");
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
            "{:w$}  {:6}  {}{:8}  {:34}  {}",
            i["name"].as_str().unwrap_or(""),
            i["kind"].as_str().unwrap_or("?"),
            on(i),
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

fn run_now(name: &str, wait: bool, json_out: bool, run_id: Option<String>, check_only: bool) -> Result<()> {
    let cfg = config::load()?;
    match cfg.find(name) {
        None => bail!("no automation named `{name}`"),
        Some(Slot::Invalid(i)) => bail!("`{name}` is invalid: {}", i.error),
        Some(Slot::Valid(_)) => {}
    }
    if check_only {
        return Ok(());
    }
    let req = runner::Request {
        name: name.to_string(),
        trigger: Trigger::Manual,
        run_id: run_id.unwrap_or_else(|| history::new_id(name)),
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
    if let Some(m) = &r.machine {
        println!("  machine        {m}");
    }
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
        let name = match &r.machine {
            Some(m) => format!("{}@{m}", r.automation),
            None => r.automation.clone(),
        };
        println!(
            "{:16}  {:26}  {:9}  {:16}  {:>6}  {}",
            local(run_time(r)),
            name,
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

/// Whether hc knows `machine`, given `hc machine ls`: Err for an unknown
/// one, Ok(Some(warning)) when it's known but not connected or hc couldn't
/// be asked.
fn machine_known(machine: &str, listed: &Result<Vec<serde_json::Value>, String>) -> Result<Option<String>> {
    let machines = match listed {
        Ok(m) => m,
        Err(e) => return Ok(Some(format!("could not check machine `{machine}`: {e}"))),
    };
    let Some(m) = machines.iter().find(|m| m["name"].as_str() == Some(machine)) else {
        let known: Vec<&str> = machines.iter().filter_map(|m| m["name"].as_str()).collect();
        let hint = if known.is_empty() {
            "hc has no machines; add one with `hc machine add`".to_string()
        } else {
            format!("known: {}", known.join(", "))
        };
        bail!("unknown machine `{machine}` ({hint})");
    };
    let state = m["state"].as_str().unwrap_or("unknown");
    Ok((state != "connected").then(|| format!("machine `{machine}` is {state} right now")))
}

/// For an entry that runs on a machine: the machine is known, and, when it
/// can be reached, its paths are there. Errors, then warnings.
fn check_machine(
    a: &config::Automation,
    listed: &Result<Vec<serde_json::Value>, String>,
) -> (Vec<String>, Vec<String>) {
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let Some(m) = a.machine() else { return (errors, warnings) };
    match machine_known(m, listed) {
        Err(e) => {
            errors.push(format!("{e:#}"));
            return (errors, warnings);
        }
        Ok(Some(w)) => {
            warnings.push(w);
            return (errors, warnings);
        }
        Ok(None) => {}
    }
    let host = match host::Host::connect(Some(m)) {
        Ok(h) => h,
        Err(e) => {
            warnings.push(format!("{e:#}"));
            return (errors, warnings);
        }
    };
    // The same checks as for a local entry, made on the machine.
    let mut want = |field: &str, path: Option<&String>, file: bool| {
        let Some(p) = path else { return };
        let full = host.expand(p);
        let (found, what) = if file { (host.is_file(&full), "a file") } else { (host.is_dir(&full), "a folder") };
        match found {
            Some(true) => {}
            Some(false) => errors.push(format!("{field} `{}` is not {what} on `{m}`", full.display())),
            None => warnings.push(format!("could not check {field} on `{m}`")),
        }
    };
    want("cwd", a.entry.cwd.as_ref(), false);
    want("repo", a.entry.repo.as_ref(), false);
    want("prompt_file", a.entry.prompt_file.as_ref(), true);
    (errors, warnings)
}

fn check(json_out: bool) -> Result<()> {
    let cfg = config::load()?;
    let mut invalid = Vec::new();
    let mut warnings = Vec::new();
    // Asked once, and only when some entry runs elsewhere.
    let listed = if cfg.valid().any(|a| a.machine().is_some()) {
        hc::machines().map_err(|e| format!("{e:#}"))
    } else {
        Ok(Vec::new())
    };
    for s in &cfg.slots {
        match s {
            Slot::Invalid(i) => invalid.push(json!({ "name": i.name, "error": i.error })),
            Slot::Valid(a) => {
                let (errors, warns) = check_machine(a, &listed);
                invalid.extend(errors.iter().map(|e| json!({ "name": a.name, "error": e })));
                warnings.extend(warns.iter().map(|w| json!({ "name": a.name, "warning": w })));
            }
        }
    }
    if json_out {
        println!(
            "{}",
            json!({ "ok": invalid.is_empty(), "count": cfg.slots.len(), "invalid": invalid, "warnings": warnings })
        );
    } else {
        if invalid.is_empty() {
            println!("{} automations, all valid", cfg.slots.len());
        }
        for i in &invalid {
            println!("{}: {}", i["name"].as_str().unwrap_or(""), i["error"].as_str().unwrap_or(""));
        }
        for w in &warnings {
            println!("{}: warning: {}", w["name"].as_str().unwrap_or(""), w["warning"].as_str().unwrap_or(""));
        }
    }
    if !invalid.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

fn machines(json_out: bool, probe: bool) -> Result<()> {
    let machines = hc::machines()?;
    let mut v: Vec<_> = machines.iter().map(|m| json!({ "name": m["name"], "state": m["state"] })).collect();
    if probe {
        // All at once: each is a round trip, and an offline one isn't asked.
        std::thread::scope(|s| {
            let asks: Vec<_> = v
                .iter()
                .map(|m| {
                    let name = m["name"].as_str().unwrap_or("").to_string();
                    let connected = m["state"] == "connected";
                    s.spawn(move || connected.then(|| remote::probe(&name)))
                })
                .collect();
            for (m, ask) in v.iter_mut().zip(asks) {
                m["automate_everything"] = ask.join().ok().flatten().unwrap_or(serde_json::Value::Null);
            }
        });
        remember_machines(&mut v);
    }
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    for m in &v {
        let ae = &m["automate_everything"];
        let extra = if !probe {
            String::new()
        } else if let Some(e) = ae["error"].as_str() {
            format!("  ({e})")
        } else if ae["installed"] == true {
            let sched = if ae["scheduler_running"] == true { "scheduler running" } else { "scheduler not running" };
            format!("  automate-everything {} ({sched})", ae["version"].as_str().unwrap_or("?"))
        } else if ae["installed"] == false {
            "  no automate-everything".into()
        } else {
            String::new()
        };
        println!("{}  {}{extra}", m["name"].as_str().unwrap_or(""), m["state"].as_str().unwrap_or(""));
    }
    Ok(())
}

/// Machines seen running automate-everything, kept in the state folder so
/// one that's offline now still shows up (`reachable: false`, `seen_at`)
/// instead of vanishing. One answering without it, or removed from hc, is
/// forgotten.
fn remember_machines(v: &mut [serde_json::Value]) {
    let path = paths::state_dir().join("machines.json");
    let mut seen: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let before = seen.clone();
    // A machine removed from hc is forgotten too.
    seen.retain(|name, _| v.iter().any(|m| m["name"].as_str() == Some(name)));
    for m in v.iter_mut() {
        let name = m["name"].as_str().unwrap_or("").to_string();
        let ae = &mut m["automate_everything"];
        if ae["installed"] == true {
            ae["reachable"] = json!(true);
            let mut keep = ae.clone();
            keep["seen_at"] = json!(Utc::now().format("%Y-%m-%dT%H:%M:00Z").to_string());
            ae["seen_at"] = keep["seen_at"].clone();
            seen.insert(name, keep);
        } else if ae["installed"] == false {
            seen.remove(&name);
        } else if let Some(last) = seen.get(&name) {
            // Offline, or it didn't answer: what it ran when last seen.
            let error = ae["error"].clone();
            *ae = last.clone();
            ae["reachable"] = json!(false);
            ae["scheduler_running"] = serde_json::Value::Null;
            if !error.is_null() {
                ae["error"] = error;
            }
        }
    }
    if seen != before {
        let _ = paths::ensure_state_dirs();
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&seen).unwrap_or_default());
    }
}

fn status(json_out: bool) -> Result<()> {
    let running = lock::is_held(&paths::scheduler_lock());
    let note = lock::read_note(&paths::scheduler_lock());
    let v = json!({
        "scheduler_running": running,
        "scheduler": if running { note } else { None },
        "service_installed": service::installed(),
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": remote::PROTOCOL,
        "timezone": cron::system_tz().name(),
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
        // Not in --json: the panel polls that, and on Linux this asks loginctl.
        for w in service::warnings() {
            println!("warning    {w}");
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(m) = &cli.on {
        match &cli.cmd {
            // Moves between here and there: done from here.
            Cmd::Transfer { .. } => {}
            Cmd::Scheduler | Cmd::RunInternal { .. } => bail!("--on doesn't apply to this command"),
            _ => {
                let args = remote::strip_on(std::env::args().skip(1), m);
                std::process::exit(remote::proxy(m, &args)?);
            }
        }
    }
    match cli.cmd {
        Cmd::List { json } => list(json),
        Cmd::Run { name, wait, json, run_id, check } => run_now(&name, wait, json, run_id, check),
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
        Cmd::Move { name, to } => {
            config::move_to(&name, to)?;
            println!("moved {name} to {to}");
            Ok(())
        }
        Cmd::Delete { name, yes, keep_history } => {
            let what = if keep_history { "" } else { " and its run history" };
            if !yes && !confirm(&format!("Delete `{name}`{what}?")) {
                bail!("not deleted (pass --yes to skip the question)");
            }
            config::remove(&name)?;
            if keep_history {
                println!("deleted {name}, kept its history");
            } else {
                let n = history::delete_for(&name);
                println!("deleted {name} and {n} history records");
            }
            Ok(())
        }
        Cmd::Transfer { name, to, fields } => {
            let fields: Option<serde_json::Value> =
                fields.map(|f| serde_json::from_str(&f)).transpose().context("--fields must be a JSON object")?;
            let fields = match &fields {
                Some(v) => Some(v.as_object().ok_or_else(|| anyhow!("--fields must be a JSON object"))?),
                None => None,
            };
            let from = cli.on.as_deref().map(transfer::Side::parse).unwrap_or(transfer::Side::Here);
            let to = transfer::Side::parse(&to);
            transfer::transfer(&name, from, to.clone(), fields)?;
            match to {
                transfer::Side::Here => println!("{name} is now scheduled by this machine"),
                transfer::Side::On(m) => println!("{name} is now scheduled by {m}"),
            }
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
            if let Some(m) = obj.get("machine").and_then(|m| m.as_str()) {
                if config::valid_machine(m) {
                    if let Some(w) = machine_known(m, &hc::machines().map_err(|e| format!("{e:#}")))? {
                        eprintln!("warning: {w}");
                    }
                }
            }
            config::upsert(&name, obj, create)?;
            println!("saved {name}");
            Ok(())
        }
        Cmd::Check { json } => check(json),
        Cmd::Status { json } => status(json),
        Cmd::Machines { json, probe } => machines(json, probe),
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
