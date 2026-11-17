//! Automations defined here that run on other machines (`runs_on`): each such
//! machine gets a copy of its entries and its own scheduler fires them, so
//! they run while this one sleeps. The definition stays here; the copy there
//! can't be changed there.
//!
//! What a machine gets is always the whole set meant for it, replacing the
//! last one in one write (`_sync-in`), so nothing can end up half-moved: an
//! entry dropped from a machine's `runs_on` is gone from its next copy. A copy
//! is sent when it changes (right after a change from the CLI, and the
//! scheduler checks every minute), and again every half hour in case the
//! machine lost it. An offline machine keeps its last copy and gets the new
//! one once it's back; its runs and history stay on it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{self, LOCAL};
use crate::lock::FileLock;
use crate::{cron, paths, remote};

/// How long a copy that hasn't changed goes before it's sent again.
const RESEND: i64 = 30;

/// Where the copies this machine was sent live, one file per sender.
fn received_dir() -> PathBuf {
    paths::state_dir().join("received")
}

fn sent_file() -> PathBuf {
    paths::state_dir().join("sent.json")
}

/// This machine, as it names itself to the machines it sends copies to.
pub fn source_id() -> String {
    let mut buf = [0u8; 256];
    let raw = unsafe {
        if libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..end]).to_string()
        } else {
            String::new()
        }
    };
    clean_id(&raw)
}

/// A hostname as a file-safe id: the first label, lowercase, `[a-z0-9-_]`.
fn clean_id(host: &str) -> String {
    let first = host.split('.').next().unwrap_or("");
    let id: String = first
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    if id.is_empty() { "unknown".into() } else { id }
}

fn valid_source(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The copies other machines sent: (sender, its config text).
pub fn received() -> Vec<(String, String)> {
    let Ok(dir) = std::fs::read_dir(received_dir()) else { return Vec::new() };
    let mut out: Vec<(String, String)> = dir
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let source = p.file_name()?.to_str()?.strip_suffix(".toml")?.to_string();
            Some((source, std::fs::read_to_string(&p).ok()?))
        })
        .collect();
    out.sort();
    out
}

/// `_sync-in`: replaces what `source` sent before with `text` (no entries: removes it).
pub fn receive(source: &str, text: &str) -> Result<usize> {
    if !valid_source(source) {
        bail!("`{source}` is not a machine id");
    }
    let cfg = config::parse(text).context("the copy")?;
    let path = received_dir().join(format!("{source}.toml"));
    if cfg.slots.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        return Ok(0);
    }
    std::fs::create_dir_all(received_dir())?;
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(cfg.slots.len())
}

/// The copy each machine should hold, from this machine's config text: its
/// entries as written, without `runs_on` (there, it's theirs to fire) or
/// `machine`, with this machine's timezone written in where there's none (an
/// empty one means the scheduler's own).
pub fn copies(text: &str, tz: &str) -> Result<BTreeMap<String, String>> {
    let table: toml::Table = toml::from_str(text).context("config is not valid TOML")?;
    let mut per: BTreeMap<String, toml::Table> = BTreeMap::new();
    let Some(autos) = table.get("automations").and_then(|a| a.as_table()) else { return Ok(BTreeMap::new()) };
    for (name, value) in autos {
        let Some(entry) = value.as_table() else { continue };
        let places: Vec<String> = entry
            .get("runs_on")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        for m in places.iter().filter(|p| *p != LOCAL && config::valid_machine(p)) {
            let mut copy = entry.clone();
            copy.remove("runs_on");
            copy.remove("machine");
            if copy.get("timezone").and_then(|z| z.as_str()).unwrap_or("").is_empty() {
                copy.insert("timezone".into(), toml::Value::String(tz.to_string()));
            }
            per.entry(m.clone()).or_default().insert(name.clone(), toml::Value::Table(copy));
        }
    }
    per.into_iter()
        .map(|(m, autos)| {
            let mut doc = toml::Table::new();
            doc.insert("automations".into(), toml::Value::Table(autos));
            Ok((m, toml::to_string(&doc)?))
        })
        .collect()
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Sent {
    hash: String,
    at: Option<DateTime<Utc>>,
    /// The last attempt's failure, while it isn't through.
    error: Option<String>,
}

fn load_sent() -> BTreeMap<String, Sent> {
    std::fs::read_to_string(sent_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_sent(s: &BTreeMap<String, Sent>) {
    let _ = paths::ensure_state_dirs();
    let _ = std::fs::write(sent_file(), serde_json::to_string_pretty(s).unwrap_or_default());
}

/// FNV-1a: stable across builds, which `DefaultHasher` isn't.
fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// What `send` did for one machine.
pub struct Outcome {
    pub machine: String,
    /// None when its copy was already current and nothing was sent.
    pub result: Option<Result<usize>>,
}

/// Sends each machine its copy where it changed, failed last time, or is
/// due again. A machine that's no longer in any `runs_on` gets an empty copy,
/// which removes the last one. Nothing to do (no `runs_on` anywhere, nothing
/// sent before) costs no hc call.
pub fn send() -> Result<Vec<Outcome>> {
    let mut sent = load_sent();
    let copies = copies(&config::read_text()?, cron::system_tz().name())?;
    if copies.is_empty() && sent.is_empty() {
        return Ok(Vec::new());
    }
    paths::ensure_state_dirs()?;
    let _lock = FileLock::acquire(&paths::state_dir().join("sync.lock"))?;
    // Read again under the lock: another send may have just finished.
    sent = load_sent();
    let machines: Vec<String> = copies.keys().chain(sent.keys()).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let source = source_id();
    let now = Utc::now();
    let mut out = Vec::new();
    for m in machines {
        let text = copies.get(&m).cloned().unwrap_or_default();
        let h = hash(&text);
        let last = sent.get(&m).cloned().unwrap_or_default();
        let due = last.at.map(|t| now - t > Duration::minutes(RESEND)).unwrap_or(true);
        if last.hash == h && last.error.is_none() && !due {
            out.push(Outcome { machine: m, result: None });
            continue;
        }
        let result = send_one(&m, &source, &text);
        match &result {
            Ok(_) if text.is_empty() => {
                sent.remove(&m);
            }
            Ok(_) => {
                sent.insert(m.clone(), Sent { hash: h, at: Some(now), error: None });
            }
            Err(e) => {
                sent.insert(m.clone(), Sent { hash: last.hash, at: last.at, error: Some(format!("{e:#}")) });
            }
        }
        out.push(Outcome { machine: m, result: Some(result) });
    }
    save_sent(&sent);
    Ok(out)
}

fn send_one(machine: &str, source: &str, text: &str) -> Result<usize> {
    remote::reachable(machine)?;
    let args: Vec<String> =
        ["_sync-in", "--from", source, "--toml", text].iter().map(|s| s.to_string()).collect();
    let o = remote::call(machine, &args)?;
    if !o.ok() {
        let e = o.stderr_text();
        if e.contains("unrecognized subcommand") {
            bail!("automate-everything on `{machine}` is too old to receive automations; update it there");
        }
        bail!("{}", e.trim_start_matches("Error: "));
    }
    o.stdout_text().trim().parse().map_err(|_| anyhow!("`{machine}` gave no count"))
}

/// After a change from the CLI: sends what changed and says, on stderr, what
/// couldn't be (the scheduler keeps trying every minute).
pub fn send_and_report() {
    match send() {
        Ok(outcomes) => {
            for o in outcomes {
                if let Some(Err(e)) = o.result {
                    eprintln!("warning: `{}` doesn't have this change yet ({e:#}); it's sent once it can be reached", o.machine);
                }
            }
        }
        Err(e) => eprintln!("warning: could not send copies to other machines: {e:#}"),
    }
}

/// Per machine: when its copy was last sent and the last failure, for `status`.
pub fn sent_status() -> serde_json::Value {
    serde_json::to_value(load_sent()).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"
[automations.here]
kind = "script"
schedule = "* * * * *"
command = "true"

[automations.both]
kind = "script"
schedule = "0 * * * *"
runs_on = ["local", "lab"]
command = "~/bin/x"

[automations.lab-only]
kind = "script"
schedule = "0 8 * * *"
timezone = "Asia/Tokyo"
runs_on = ["lab", "box"]
command = "uname"
"#;

    #[test]
    fn each_machine_gets_its_entries_without_runs_on_and_with_a_timezone() {
        let c = copies(TEXT, "Europe/Madrid").unwrap();
        assert_eq!(c.keys().collect::<Vec<_>>(), ["box", "lab"]);
        let lab = config::parse(&c["lab"]).unwrap();
        let names: Vec<_> = lab.slots.iter().map(|s| s.name().to_string()).collect();
        assert_eq!(names, ["both", "lab-only"]);
        let t: toml::Table = toml::from_str(&c["lab"]).unwrap();
        let both = t["automations"]["both"].as_table().unwrap();
        assert!(both.get("runs_on").is_none());
        assert_eq!(both["timezone"].as_str(), Some("Europe/Madrid"));
        assert_eq!(t["automations"]["lab-only"]["timezone"].as_str(), Some("Asia/Tokyo"));
        // There, a copy is fired by that machine itself.
        for s in &lab.slots {
            match s {
                config::Slot::Valid(a) => assert!(a.runs_here()),
                config::Slot::Invalid(i) => panic!("{}", i.error),
            }
        }
    }

    #[test]
    fn nothing_to_send_without_runs_on() {
        assert!(copies("[automations.a]\nkind = \"script\"\n", "UTC").unwrap().is_empty());
        assert!(copies("", "UTC").unwrap().is_empty());
    }

    #[test]
    fn source_ids_are_file_safe() {
        assert_eq!(clean_id("Sergis-MacBook-Pro.local"), "sergis-macbook-pro");
        assert_eq!(clean_id("lab"), "lab");
        assert_eq!(clean_id(""), "unknown");
        assert!(valid_source("sergis-macbook-pro"));
        assert!(!valid_source("../etc"));
    }

    #[test]
    fn the_hash_is_stable() {
        assert_eq!(hash(""), "cbf29ce484222325");
        assert_ne!(hash("a"), hash("b"));
    }
}
