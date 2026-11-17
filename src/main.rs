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
mod sync;
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
        /// Run it on this machine only, whatever its runs_on
        #[arg(long, hide = true)]
        here: bool,
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
    /// Remove the entry from the config and delete its history (here; a
    /// machine in its runs_on keeps its own)
    Delete {
        name: String,
        #[arg(long)]
        yes: bool,
        /// Remove only the entry, keeping its run history
        #[arg(long)]
        keep_history: bool,
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
        /// (version, scheduler), so automations can run there (runs_on)
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
    /// Replaces the automations another machine sent this one (its
    /// `runs_on`); the scheduler here fires them
    #[command(name = "_sync-in", hide = true)]
    SyncIn {
        #[arg(long)]
        from: String,
        #[arg(long)]
        toml: String,
    },
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
                    "runs_on": a.runs_on(), "origin": a.origin,
                    "next": next, "last": last, "live": live,
                })
            }
            Slot::Invalid(i) => json!({
                "name": name, "kind": i.kind, "status": if running { "running" } else { "invalid" }, "paused": i.paused,
                "schedule": i.schedule, "timezone": null, "description": null, "machine": i.machine, "error": i.error,
                "runs_on": null, "origin": i.origin,
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
    // The WHERE column only shows once some entry runs elsewhere: its
    // machine (fired from here), or its runs_on.
    let place = |i: &serde_json::Value| -> Option<String> {
        if let Some(m) = i["machine"].as_str() {
            return Some(format!("{m} (via here)"));
        }
        let places: Vec<&str> = i["runs_on"].as_array()?.iter().filter_map(|p| p.as_str()).collect();
        (places != [config::LOCAL]).then(|| places.join(","))
    };
    let mw = items.iter().filter_map(place).map(|p| p.len()).max().map(|n| n.max(5));
    let on = |i: &serde_json::Value| match mw {
        Some(mw) => format!("{:mw$}  ", place(i).unwrap_or_else(|| "local".into())),
        None => String::new(),
    };
    let header = mw.map(|mw| format!("{:mw$}  ", "WHERE")).unwrap_or_default();
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

/// Run now, wherever it runs (`runs_on`): here in the background (or
/// waiting, with `--wait`), and on each machine in a terminal there.
fn run_now(name: &str, wait: bool, json_out: bool, run_id: Option<String>, check_only: bool, here: bool) -> Result<()> {
    let cfg = config::load()?;
    let places = match cfg.find(name) {
        None => bail!("no automation named `{name}`"),
        Some(Slot::Invalid(i)) => bail!("`{name}` is invalid: {}", i.error),
        Some(Slot::Valid(a)) => a.runs_on(),
    };
    if check_only {
        return Ok(());
    }
    let machines: Vec<&String> = if here { Vec::new() } else { places.iter().filter(|p| *p != config::LOCAL).collect() };
    let local = here || places.iter().any(|p| p == config::LOCAL);
    if !machines.is_empty() {
        if wait {
            match (local, machines.as_slice()) {
                (false, [m]) => {
                    let args: Vec<String> = ["run", name, "--wait"].iter().map(|s| s.to_string()).chain(json_out.then(|| "--json".to_string())).collect();
                    std::process::exit(remote::proxy(m, &args)?);
                }
                _ => bail!("`{name}` runs in more than one place; --wait needs one: add --on <machine>, or run it without --wait"),
            }
        }
        let mut runs = Vec::new();
        let mut errors = Vec::new();
        if local {
            let req = runner::Request {
                name: name.to_string(),
                trigger: Trigger::Manual,
                run_id: run_id.clone().unwrap_or_else(|| history::new_id(name)),
                scheduled_for: None,
                caught_up: None,
            };
            paths::ensure_state_dirs()?;
            let mut child = runner::spawn(&req)?;
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = child.try_wait();
            runs.push(json!({ "on": null, "run_id": req.run_id }));
        }
        // Asked once for all of them.
        let listed = hc::machines();
        for m in machines {
            let ready = match &listed {
                Ok(l) => remote::reachable_in(l, m),
                Err(e) => Err(anyhow!("{e:#}")),
            };
            match ready.and_then(|_| remote::start_run(m, name)) {
                Ok(id) => runs.push(json!({ "on": m, "run_id": id })),
                Err(e) => errors.push(format!("{m}: {e:#}")),
            }
        }
        if runs.is_empty() {
            bail!("{}", errors.join("; "));
        }
        if json_out {
            println!("{}", json!({ "run_id": runs[0]["run_id"], "runs": runs, "errors": errors }));
        } else {
            for r in &runs {
                let on = r["on"].as_str().map(|m| format!(" on {m}")).unwrap_or_default();
                println!("started {name}{on} ({})", r["run_id"].as_str().unwrap_or(""));
            }
            for e in &errors {
                eprintln!("not started on {e}");
            }
        }
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

/// The copies this machine sent, as each machine in a runs_on sees them: its
/// paths and fields can only be judged there. Invalid ones, then machines
/// that couldn't be asked.
fn check_copies(
    cfg: &config::Config,
    listed: &Result<Vec<serde_json::Value>, String>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let (mut invalid, mut warnings) = (Vec::new(), Vec::new());
    let mut machines: Vec<String> = cfg
        .valid()
        .filter(|a| a.origin.is_none())
        .flat_map(|a| a.runs_on())
        .filter(|p| p != config::LOCAL)
        .collect();
    machines.sort();
    machines.dedup();
    let source = sync::source_id();
    for m in machines {
        let asked = match listed {
            Ok(l) => remote::reachable_in(l, &m).and_then(|_| remote::call(&m, &["list".into(), "--json".into()])),
            Err(e) => Err(anyhow!("{e}")),
        };
        let items: Vec<serde_json::Value> = match asked {
            Ok(o) if o.ok() => serde_json::from_slice(&o.stdout).unwrap_or_default(),
            Ok(o) => {
                warnings.push(json!({ "name": m, "warning": format!("could not check its copies on `{m}`: {}", o.stderr_text()) }));
                continue;
            }
            Err(e) => {
                warnings.push(json!({ "name": m, "warning": format!("could not check its copies on `{m}`: {e:#}") }));
                continue;
            }
        };
        for i in items.iter().filter(|i| i["origin"].as_str() == Some(source.as_str())) {
            if let Some(e) = i["error"].as_str() {
                invalid.push(json!({ "name": i["name"], "error": format!("on `{m}`: {e}"), "machine": m }));
            }
        }
    }
    (invalid, warnings)
}

fn check(json_out: bool) -> Result<()> {
    let cfg = config::load()?;
    let mut invalid = Vec::new();
    let mut warnings = Vec::new();
    // Asked once, and only when some entry runs elsewhere.
    let elsewhere = |a: &config::Automation| a.machine().is_some() || !a.runs_here() || a.runs_on().len() > 1;
    let listed = if cfg.valid().any(elsewhere) {
        hc::machines().map_err(|e| format!("{e:#}"))
    } else {
        Ok(Vec::new())
    };
    for s in &cfg.slots {
        match s {
            Slot::Invalid(i) => invalid.push(json!({ "name": i.name, "error": i.error })),
            Slot::Valid(a) => {
                let (mut errors, mut warns) = check_machine(a, &listed);
                for m in a.runs_on().iter().filter(|p| *p != config::LOCAL) {
                    match machine_known(m, &listed) {
                        Err(e) => errors.push(format!("runs_on: {e:#}")),
                        Ok(Some(w)) => warns.push(format!("runs_on: {w}")),
                        Ok(None) => {}
                    }
                }
                invalid.extend(errors.iter().map(|e| json!({ "name": a.name, "error": e })));
                warnings.extend(warns.iter().map(|w| json!({ "name": a.name, "warning": w })));
            }
        }
    }
    let (bad, warns) = check_copies(&cfg, &listed);
    invalid.extend(bad);
    warnings.extend(warns);
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

/// Changes go to where an automation is defined: a copy another machine sent
/// is changed there.
fn not_a_copy(name: &str) -> Result<()> {
    if let Some(origin) = config::load()?.find(name).and_then(|s| s.origin().map(String::from)) {
        bail!("`{name}` is defined on `{origin}`, which sends it here: change it there");
    }
    Ok(())
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
        "source_id": sync::source_id(),
        "sent": sync::sent_status(),
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
            Cmd::Scheduler | Cmd::RunInternal { .. } | Cmd::SyncIn { .. } => bail!("--on doesn't apply to this command"),
            _ => {
                let args = remote::strip_on(std::env::args().skip(1), m);
                std::process::exit(remote::proxy(m, &args)?);
            }
        }
    }
    match cli.cmd {
        Cmd::List { json } => list(json),
        Cmd::Run { name, wait, json, run_id, check, here } => run_now(&name, wait, json, run_id, check, here),
        Cmd::Pause { name } => {
            not_a_copy(&name)?;
            config::set_paused(&name, true)?;
            println!("paused {name}");
            sync::send_and_report();
            Ok(())
        }
        Cmd::Resume { name } => {
            not_a_copy(&name)?;
            config::set_paused(&name, false)?;
            println!("resumed {name}");
            sync::send_and_report();
            Ok(())
        }
        Cmd::Move { name, to } => {
            not_a_copy(&name)?;
            config::move_to(&name, to)?;
            println!("moved {name} to {to}");
            Ok(())
        }
        Cmd::Delete { name, yes, keep_history } => {
            not_a_copy(&name)?;
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
            sync::send_and_report();
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
            let mut named: Vec<&str> = obj.get("machine").and_then(|m| m.as_str()).into_iter().collect();
            if let Some(places) = obj.get("runs_on").and_then(|v| v.as_array()) {
                named.extend(places.iter().filter_map(|p| p.as_str()).filter(|p| *p != config::LOCAL));
            }
            if named.iter().any(|m| config::valid_machine(m)) {
                let listed = hc::machines().map_err(|e| format!("{e:#}"));
                for m in named.iter().filter(|m| config::valid_machine(m)) {
                    if let Some(w) = machine_known(m, &listed)? {
                        eprintln!("warning: {w}");
                    }
                }
            }
            not_a_copy(&name)?;
            config::upsert(&name, obj, create)?;
            println!("saved {name}");
            sync::send_and_report();
            Ok(())
        }
        Cmd::Check { json } => check(json),
        Cmd::Status { json } => status(json),
        Cmd::Machines { json, probe } => machines(json, probe),
        Cmd::Service { action } => service::run(action),
        Cmd::Tab { action } => tabs::run(action),
        Cmd::Scheduler => scheduler::serve(),
        Cmd::SyncIn { from, toml } => {
            println!("{}", sync::receive(&from, &toml)?);
            Ok(())
        }
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
