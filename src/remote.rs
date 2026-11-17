//! `--on <machine>`: the same command, answered by automate-everything on
//! another herdr-core machine (through `hc exec`), against that machine's own
//! config, history and scheduler.
//!
//! Never hangs: a machine hc doesn't show as connected fails at once, and
//! every call has a short timeout.
//!
//! `hc exec` ends everything its program started when it returns, so a run
//! started there with `run` (no `--wait`) goes in a terminal of its own
//! (`hc run --bg --rm`) instead.

use std::io::Write;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::{hc, history};

/// What `--on` and `runs_on` need from the other side; reported by `status
/// --json` as `protocol`. Older builds report none (0); 2 receives copies
/// (`_sync-in`).
pub const PROTOCOL: u64 = 2;

/// Exit code of `WRAP` when the machine has no automate-everything.
const NOT_INSTALLED: i32 = 127;

/// automate-everything from the machine's ~/.local/bin (where `service
/// ensure` and install.sh put it), its arguments after.
const WRAP: &str = r#"b="$HOME/.local/bin/automate-everything"
[ -x "$b" ] || { echo "automate-everything is not installed in ~/.local/bin" >&2; exit 127; }
exec "$b" "$@""#;

/// How long hc itself may take on top of the command's own timeout.
const GRACE: Duration = Duration::from_secs(5);

/// The subcommand in `args` (the first word that isn't a flag).
fn subcommand(args: &[String]) -> &str {
    args.iter().find(|a| !a.starts_with('-')).map(String::as_str).unwrap_or("")
}

/// Seconds a command may take there: reads are quick, `run --wait` takes as
/// long as hc allows.
pub fn timeout_for(args: &[String]) -> u64 {
    match subcommand(args) {
        "run" if args.iter().any(|a| a == "--wait") => 600,
        "service" => 300,
        "list" | "status" | "history" | "show" | "get" | "machines" => 10,
        _ => 30,
    }
}

/// `args` without the `--on <machine>` (or `--on=<machine>`) that sent them.
pub fn strip_on(args: impl IntoIterator<Item = String>, machine: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.into_iter().peekable();
    let mut done = false;
    while let Some(a) = it.next() {
        if !done && a == "--on" && it.peek().map(String::as_str) == Some(machine) {
            it.next();
            done = true;
        } else if !done && a == format!("--on={machine}") {
            done = true;
        } else {
            out.push(a);
        }
    }
    out
}

/// Errs unless hc knows `machine` and shows it connected: what keeps an
/// offline machine from costing a timeout.
pub fn reachable(machine: &str) -> Result<()> {
    let machines = hc::machines()?;
    let Some(m) = machines.iter().find(|m| m["name"].as_str() == Some(machine)) else {
        let known: Vec<&str> = machines.iter().filter_map(|m| m["name"].as_str()).collect();
        bail!("unknown machine `{machine}` (known: {})", if known.is_empty() { "none".into() } else { known.join(", ") });
    };
    let state = m["state"].as_str().unwrap_or("unknown");
    match state {
        "connected" => {}
        "unreachable" => bail!("machine `{machine}` is unreachable"),
        _ => bail!("machine `{machine}` is unreachable ({state})"),
    }
    Ok(())
}

/// Runs automate-everything `args` on `machine`. Errs when it couldn't be
/// asked or didn't answer in time; its own failure is in the result.
pub fn call(machine: &str, args: &[String]) -> Result<hc::ExecOut> {
    let timeout = timeout_for(args);
    let mut argv = vec!["/bin/sh", "-c", WRAP, "sh"];
    argv.extend(args.iter().map(String::as_str));
    let out = hc::exec_on_within(machine, &argv, timeout, GRACE)
        .map_err(|e| anyhow!("machine `{machine}` unreachable: {e:#}"))?;
    if out.timed_out {
        bail!("machine `{machine}` didn't answer within {timeout}s");
    }
    if out.exit_code == Some(NOT_INSTALLED) {
        bail!("automate-everything is not installed on `{machine}`");
    }
    Ok(out)
}

/// The command line given with `--on`, answered there; its output is printed
/// here and its exit code returned.
pub fn proxy(machine: &str, args: &[String]) -> Result<i32> {
    reachable(machine)?;
    if subcommand(args) == "run" && !args.iter().any(|a| a == "--wait" || a == "--check") {
        return run_detached(machine, args);
    }
    let out = call(machine, args)?;
    std::io::stdout().write_all(&out.stdout)?;
    std::io::stderr().write_all(&out.stderr)?;
    Ok(out.exit_code.unwrap_or(1))
}

/// `run <name>` there: checked with `run --check`, then run in a terminal of
/// its own that's removed when it ends.
fn run_detached(machine: &str, args: &[String]) -> Result<i32> {
    let json_out = args.iter().any(|a| a == "--json");
    let name = args
        .iter()
        .skip_while(|a| a.as_str() != "run")
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("run: which automation?"))?
        .clone();
    let run_id = start_run(machine, &name)?;
    if json_out {
        println!("{}", json!({ "run_id": run_id, "runs": [{ "on": machine, "run_id": run_id }] }));
    } else {
        println!("started {name} on {machine} ({run_id})");
    }
    Ok(0)
}

/// Starts `name` on `machine` (its own entry there, or the copy it was
/// sent), not waiting for it; its run id.
pub fn start_run(machine: &str, name: &str) -> Result<String> {
    let check = call(machine, &["run".into(), name.into(), "--check".into()])?;
    if !check.ok() {
        bail!("{}", check.stderr_text().trim_start_matches("Error: "));
    }
    let run_id = history::new_id(name);
    let mut argv: Vec<String> = ["/bin/sh", "-c", WRAP, "sh", "run"].iter().map(|s| s.to_string()).collect();
    argv.extend([name.to_string(), "--wait".into(), "--here".into(), "--run-id".into(), run_id.clone()]);
    hc::launch(&hc::Launch {
        on: Some(machine),
        cwd: "/",
        labels: vec![("automation".into(), name.to_string())],
        argv,
        keep: false,
        rm: true,
    })
    .map_err(|e| anyhow!("machine `{machine}` unreachable: {e:#}"))?;
    Ok(run_id)
}

/// What `machine` runs: `installed`, and when it is, its `version`,
/// `protocol`, `scheduler_running` and `timezone`; `error` when it couldn't
/// be asked.
pub fn probe(machine: &str) -> Value {
    let args = ["status".to_string(), "--json".to_string()];
    match call(machine, &args) {
        Err(e) if format!("{e:#}").contains("not installed") => json!({ "installed": false }),
        Err(e) => json!({ "error": format!("{e:#}") }),
        Ok(o) if !o.ok() => json!({ "error": o.stderr_text() }),
        Ok(o) => {
            let s: Value = serde_json::from_str(&o.stdout_text()).unwrap_or(Value::Null);
            json!({
                "installed": true,
                "version": s["version"],
                "protocol": s["protocol"].as_u64().unwrap_or(0),
                "scheduler_running": s["scheduler_running"],
                "timezone": s["timezone"],
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn strip_on_removes_only_the_flag_that_names_the_machine() {
        assert_eq!(strip_on(v(&["--on", "lab", "list", "--json"]), "lab"), v(&["list", "--json"]));
        assert_eq!(strip_on(v(&["list", "--on=lab"]), "lab"), v(&["list"]));
        assert_eq!(strip_on(v(&["pause", "x", "--on", "lab"]), "lab"), v(&["pause", "x"]));
        // A value that happens to read `--on` stays.
        assert_eq!(
            strip_on(v(&["--on", "lab", "set", "x", "--fields", "--on"]), "lab"),
            v(&["set", "x", "--fields", "--on"])
        );
    }

    #[test]
    fn reads_get_short_timeouts_and_waits_long_ones() {
        assert_eq!(timeout_for(&v(&["list", "--json"])), 10);
        assert_eq!(timeout_for(&v(&["--json", "status"])), 10);
        assert_eq!(timeout_for(&v(&["set", "x", "--fields", "{}"])), 30);
        assert_eq!(timeout_for(&v(&["run", "x", "--wait"])), 600);
    }
}
