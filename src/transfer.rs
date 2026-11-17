//! `transfer`: hands an entry to another scheduler, this machine's or another
//! herdr-core machine's, so it's that one that keeps the schedule and fires it.
//!
//! The order keeps it from ever firing from both, or from being in neither:
//! create it on the target paused, remove it from the source (keeping the
//! source's history), then resume it on the target if it wasn't paused. When
//! the removal fails, the copy on the target is removed again. A crash in
//! between leaves at worst a paused copy on the target.

use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Result};
use serde_json::{Map, Value};

use crate::{cron, paths, remote};

#[derive(Debug, Clone, PartialEq)]
pub enum Side {
    /// This machine.
    Here,
    /// Another herdr-core machine, by its hc name.
    On(String),
}

impl Side {
    /// `here` for this machine, else a machine name.
    pub fn parse(s: &str) -> Side {
        if s == "here" {
            Side::Here
        } else {
            Side::On(s.to_string())
        }
    }

    fn label(&self) -> String {
        match self {
            Side::Here => "this machine".into(),
            Side::On(m) => format!("`{m}`"),
        }
    }

    /// automate-everything `args` on this side; its stdout, or its error.
    fn cli(&self, args: &[&str]) -> Result<String> {
        let (ok, stdout, stderr) = match self {
            Side::Here => {
                let o = Command::new(paths::current_exe()?).args(args).stdin(Stdio::null()).output()?;
                (o.status.success(), o.stdout, o.stderr)
            }
            Side::On(m) => {
                let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                let o = remote::call(m, &args)?;
                (o.ok(), o.stdout, o.stderr)
            }
        };
        if !ok {
            let e = String::from_utf8_lossy(&stderr);
            bail!("{} on {}", e.trim().trim_start_matches("Error: "), self.label());
        }
        Ok(String::from_utf8_lossy(&stdout).to_string())
    }

    /// Reachable, with a build that understands what transfer asks of it;
    /// gives its system timezone.
    fn ready(&self) -> Result<String> {
        match self {
            Side::Here => Ok(cron::system_tz().name().to_string()),
            Side::On(m) => {
                remote::reachable(m)?;
                let p = remote::probe(m);
                if let Some(e) = p["error"].as_str() {
                    bail!("{e}");
                }
                if p["installed"] != true {
                    bail!("automate-everything is not installed on `{m}`");
                }
                if p["protocol"].as_u64().unwrap_or(0) < remote::PROTOCOL {
                    let v = p["version"].as_str().unwrap_or("an older version");
                    bail!("`{m}` runs automate-everything {v}; update it there first");
                }
                p["timezone"].as_str().map(String::from).ok_or_else(|| anyhow!("`{m}` gave no timezone"))
            }
        }
    }
}

/// The entry as the target will hold it: `fields` applied (null removes),
/// the source's timezone written in when it had none (an empty one means the
/// scheduler's own), and `machine` made relative to the target.
pub fn relocate(
    name: &str,
    mut entry: Map<String, Value>,
    fields: &Map<String, Value>,
    from: &Side,
    to: &Side,
    from_tz: &str,
) -> Result<Map<String, Value>> {
    for (k, v) in fields {
        if v.is_null() {
            entry.remove(k);
        } else {
            entry.insert(k.clone(), v.clone());
        }
    }
    if entry.get("timezone").and_then(Value::as_str).unwrap_or("").is_empty() {
        entry.insert("timezone".into(), Value::String(from_tz.to_string()));
    }
    let machine = entry.get("machine").and_then(Value::as_str).map(String::from);
    match (from, to) {
        (Side::Here, Side::On(m)) => {
            if machine.as_deref() != Some(m.as_str()) {
                let now = machine.map(|x| format!("on `{x}`")).unwrap_or_else(|| "here".into());
                bail!("`{name}` runs {now}; one scheduled by `{m}` runs on `{m}`: set its machine to `{m}` too");
            }
            // On the machine it's a local entry.
            entry.remove("machine");
        }
        (Side::On(m), Side::Here) => {
            if machine.is_none() {
                entry.insert("machine".into(), Value::String(m.clone()));
            }
        }
        _ => bail!("transfer goes between this machine and another one"),
    }
    Ok(entry)
}

pub fn transfer(name: &str, from: Side, to: Side, fields: Option<&Map<String, Value>>) -> Result<()> {
    if from == to {
        bail!("`{name}` is already scheduled by {}", to.label());
    }
    let from_tz = from.ready()?;
    to.ready()?;
    let text = from.cli(&["get", name])?;
    let entry: Map<String, Value> = serde_json::from_str(&text)?;
    let paused = entry.get("paused").and_then(Value::as_bool).unwrap_or(false);
    let mut entry = relocate(name, entry, fields.unwrap_or(&Map::new()), &from, &to, &from_tz)?;
    entry.insert("paused".into(), Value::Bool(true));

    to.cli(&["set", name, "--create", "--fields", &Value::Object(entry).to_string()])?;
    if let Err(e) = from.cli(&["delete", name, "--yes", "--keep-history"]) {
        return match to.cli(&["delete", name, "--yes"]) {
            Ok(_) => Err(e.context(format!("not moved: `{name}` stays scheduled by {}", from.label()))),
            Err(e2) => Err(e.context(format!(
                "`{name}` is now on both, paused on {}: delete it there ({e2:#})",
                to.label()
            ))),
        };
    }
    if !paused {
        to.cli(&["resume", name])
            .map_err(|e| e.context(format!("moved `{name}` to {}, but it's still paused there", to.label())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn to_a_machine_drops_machine_and_pins_the_timezone() {
        let e = obj(json!({ "kind": "script", "schedule": "* * * * *", "machine": "lab" }));
        let out = relocate("p", e, &Map::new(), &Side::Here, &Side::On("lab".into()), "Europe/Madrid").unwrap();
        assert_eq!(Value::Object(out), json!({ "kind": "script", "schedule": "* * * * *", "timezone": "Europe/Madrid" }));
    }

    #[test]
    fn to_a_machine_needs_the_entry_to_run_there() {
        let e = obj(json!({ "kind": "script" }));
        let err = relocate("p", e, &Map::new(), &Side::Here, &Side::On("lab".into()), "UTC").unwrap_err();
        assert!(format!("{err}").contains("set its machine to `lab`"));
        // The fields given count: the form sends Runs on with Scheduled by.
        let e = obj(json!({ "kind": "script" }));
        let f = obj(json!({ "machine": "lab", "schedule": "0 * * * *" }));
        let out = relocate("p", e, &f, &Side::Here, &Side::On("lab".into()), "UTC").unwrap();
        assert_eq!(out.get("machine"), None);
        assert_eq!(out["schedule"], "0 * * * *");
    }

    #[test]
    fn back_from_a_machine_runs_there_and_keeps_its_timezone() {
        let e = obj(json!({ "kind": "script", "timezone": "Asia/Tokyo", "description": "x" }));
        let f = obj(json!({ "description": null }));
        let out = relocate("p", e, &f, &Side::On("lab".into()), &Side::Here, "UTC").unwrap();
        assert_eq!(Value::Object(out), json!({ "kind": "script", "timezone": "Asia/Tokyo", "machine": "lab" }));
    }

    #[test]
    fn only_between_here_and_a_machine() {
        let e = obj(json!({}));
        assert!(relocate("p", e, &Map::new(), &Side::On("a".into()), &Side::On("b".into()), "UTC").is_err());
    }
}
