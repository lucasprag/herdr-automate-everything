//! The config file: one TOML file, one `[automations.<name>]` table per entry.
//!
//! Reads go through `toml` (typed, per entry, so one bad entry doesn't hide
//! the rest). Writes go through `toml_edit` on the file's own text, touching
//! only the one entry being changed: comments, order and every other entry
//! stay byte-for-byte as they were.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::cron::{system_tz, Cron};
use crate::lock::FileLock;
use crate::paths;

pub const DEFAULT_GRACE_MINUTES: u64 = 10;
pub const DEFAULT_SCRIPT_TIMEOUT: u64 = 60 * 60;
pub const DEFAULT_AGENT_TIMEOUT: u64 = 3 * 60 * 60;
pub const DEFAULT_PRECHECK_TIMEOUT: u64 = 120;
pub const DEFAULT_RETENTION_DAYS: u64 = 30;
pub const DEFAULT_MAX_RUNS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Script,
    Agent,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Script => "script",
            Kind::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Directory {
    #[default]
    Repo,
    Worktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SpaceMode {
    /// A new space per run.
    #[default]
    PerRun,
    /// One space, by name, that collects runs as tabs.
    Shared,
}

/// One entry exactly as written.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: Option<Kind>,
    pub schedule: Option<String>,
    pub description: Option<String>,
    pub timezone: Option<String>,
    pub grace_minutes: Option<u64>,
    pub paused: Option<bool>,
    pub timeout_seconds: Option<u64>,
    pub env: Option<BTreeMap<String, String>>,

    // script
    pub command: Option<String>,
    pub cwd: Option<String>,

    // agent
    pub agent: Option<String>,
    pub model: Option<String>,
    pub agent_args: Option<Vec<String>>,
    pub repo: Option<String>,
    pub prompt: Option<String>,
    pub prompt_file: Option<String>,
    pub directory: Option<Directory>,
    pub base_branch: Option<String>,
    pub space: Option<SpaceMode>,
    pub space_name: Option<String>,
    pub precheck: Option<String>,
    pub precheck_timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Automation {
    pub name: String,
    pub entry: Entry,
    pub kind: Kind,
    pub cron: Cron,
    pub tz: Tz,
}

impl Automation {
    pub fn paused(&self) -> bool {
        self.entry.paused.unwrap_or(false)
    }
    pub fn grace_minutes(&self) -> u64 {
        self.entry.grace_minutes.unwrap_or(DEFAULT_GRACE_MINUTES)
    }
    pub fn timeout_seconds(&self) -> u64 {
        self.entry.timeout_seconds.unwrap_or(match self.kind {
            Kind::Script => DEFAULT_SCRIPT_TIMEOUT,
            Kind::Agent => DEFAULT_AGENT_TIMEOUT,
        })
    }
    pub fn precheck_timeout_seconds(&self) -> u64 {
        self.entry.precheck_timeout_seconds.unwrap_or(DEFAULT_PRECHECK_TIMEOUT)
    }
    pub fn env(&self) -> BTreeMap<String, String> {
        self.entry.env.clone().unwrap_or_default()
    }
    pub fn agent(&self) -> &str {
        self.entry.agent.as_deref().unwrap_or("claude")
    }
    pub fn directory(&self) -> Directory {
        self.entry.directory.unwrap_or_default()
    }
    pub fn base_branch(&self) -> String {
        self.entry.base_branch.clone().unwrap_or_else(|| "origin/main".into())
    }
    pub fn space_mode(&self) -> SpaceMode {
        self.entry.space.unwrap_or_default()
    }
    pub fn space_name(&self) -> String {
        self.entry
            .space_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.name.clone())
    }
    pub fn repo(&self) -> Option<PathBuf> {
        self.entry.repo.as_deref().map(paths::expand)
    }
    /// The script's working directory: `cwd`, else home.
    pub fn script_cwd(&self) -> PathBuf {
        self.entry.cwd.as_deref().map(paths::expand).unwrap_or_else(paths::home)
    }
    /// The prompt text: `prompt`, else the contents of `prompt_file`.
    pub fn prompt_text(&self) -> Result<String> {
        if let Some(p) = &self.entry.prompt {
            return Ok(p.clone());
        }
        let file = self.entry.prompt_file.as_deref().ok_or_else(|| anyhow!("no prompt"))?;
        let path = paths::expand(file);
        std::fs::read_to_string(&path).with_context(|| format!("reading prompt_file {}", path.display()))
    }
}

#[derive(Debug, Clone)]
pub struct Invalid {
    pub name: String,
    pub error: String,
    pub kind: Option<String>,
    pub schedule: Option<String>,
    pub paused: bool,
}

#[derive(Debug, Clone)]
pub enum Slot {
    Valid(Automation),
    Invalid(Invalid),
}

impl Slot {
    pub fn name(&self) -> &str {
        match self {
            Slot::Valid(a) => &a.name,
            Slot::Invalid(i) => &i.name,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub retention_days: u64,
    pub max_runs_per_automation: usize,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub settings: Settings,
    pub slots: Vec<Slot>,
}

impl Config {
    pub fn find(&self, name: &str) -> Option<&Slot> {
        self.slots.iter().find(|s| s.name() == name)
    }
    pub fn valid(&self) -> impl Iterator<Item = &Automation> {
        self.slots.iter().filter_map(|s| match s {
            Slot::Valid(a) => Some(a),
            _ => None,
        })
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !name.starts_with('.')
}

const AGENT_ONLY: &[&str] = &[
    "agent", "model", "agent_args", "repo", "prompt", "prompt_file", "directory", "base_branch", "space",
    "space_name", "precheck", "precheck_timeout_seconds",
];
const SCRIPT_ONLY: &[&str] = &["command", "cwd"];

/// The order new fields are written in.
const FIELD_ORDER: &[&str] = &[
    "kind", "description", "schedule", "timezone", "grace_minutes", "paused", "command", "cwd", "agent", "model",
    "agent_args", "repo", "directory", "base_branch", "space", "space_name", "prompt", "prompt_file", "precheck",
    "precheck_timeout_seconds", "timeout_seconds", "env",
];

fn set_fields(entry: &Entry) -> Vec<&'static str> {
    let mut v = Vec::new();
    macro_rules! check {
        ($($f:ident),*) => { $( if entry.$f.is_some() { v.push(stringify!($f)); } )* };
    }
    check!(
        agent, model, agent_args, repo, prompt, prompt_file, directory, base_branch, space, space_name, precheck,
        precheck_timeout_seconds, command, cwd
    );
    v
}

pub fn validate(name: &str, entry: Entry) -> Result<Automation> {
    if !valid_name(name) {
        bail!("name must be letters, digits, `-`, `_` or `.`");
    }
    let kind = entry.kind.ok_or_else(|| anyhow!("`kind` is required (script or agent)"))?;
    let schedule = entry.schedule.as_deref().ok_or_else(|| anyhow!("`schedule` is required"))?;
    let cron = Cron::parse(schedule).with_context(|| format!("schedule `{schedule}`"))?;
    let tz = match entry.timezone.as_deref() {
        Some(z) if !z.is_empty() => z.parse::<Tz>().map_err(|_| anyhow!("unknown timezone `{z}`"))?,
        _ => system_tz(),
    };
    if let Some(env) = &entry.env {
        for k in env.keys() {
            if k.is_empty() || k.contains('=') {
                bail!("env name `{k}` is invalid");
            }
        }
    }
    let fields = set_fields(&entry);
    match kind {
        Kind::Script => {
            if let Some(f) = fields.iter().find(|f| AGENT_ONLY.contains(f)) {
                bail!("`{f}` is for agent automations only");
            }
            match entry.command.as_deref() {
                Some(c) if !c.trim().is_empty() => {}
                _ => bail!("a script automation needs `command`"),
            }
            if let Some(cwd) = &entry.cwd {
                let p = paths::expand(cwd);
                if !p.is_dir() {
                    bail!("cwd `{}` is not a folder", p.display());
                }
            }
        }
        Kind::Agent => {
            if let Some(f) = fields.iter().find(|f| SCRIPT_ONLY.contains(f)) {
                bail!("`{f}` is for script automations only");
            }
            let repo = entry.repo.as_deref().ok_or_else(|| anyhow!("an agent automation needs `repo`"))?;
            let rp = paths::expand(repo);
            if !rp.is_dir() {
                bail!("repo `{}` is not a folder", rp.display());
            }
            match (&entry.prompt, &entry.prompt_file) {
                (Some(_), Some(_)) => bail!("set `prompt` or `prompt_file`, not both"),
                (None, None) => bail!("an agent automation needs `prompt` or `prompt_file`"),
                (Some(p), None) if p.trim().is_empty() => bail!("`prompt` is empty"),
                (None, Some(f)) => {
                    let p = paths::expand(f);
                    if !p.is_file() {
                        bail!("prompt_file `{}` does not exist", p.display());
                    }
                }
                _ => {}
            }
            if entry.directory == Some(Directory::Repo) && entry.base_branch.is_some() {
                bail!("`base_branch` only applies with directory = \"worktree\"");
            }
            if matches!(&entry.agent, Some(a) if a.trim().is_empty()) {
                bail!("`agent` is empty");
            }
        }
    }
    Ok(Automation { name: name.to_string(), entry, kind, cron, tz })
}

fn str_field(v: &toml::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(String::from)
}

pub fn parse(text: &str) -> Result<Config> {
    let table: toml::Table = toml::from_str(text).context("config is not valid TOML")?;
    let mut settings = Settings {
        retention_days: DEFAULT_RETENTION_DAYS,
        max_runs_per_automation: DEFAULT_MAX_RUNS,
    };
    if let Some(s) = table.get("settings").and_then(|s| s.as_table()) {
        if let Some(d) = s.get("retention_days").and_then(|v| v.as_integer()) {
            settings.retention_days = d.max(1) as u64;
        }
        if let Some(m) = s.get("max_runs_per_automation").and_then(|v| v.as_integer()) {
            settings.max_runs_per_automation = m.max(1) as usize;
        }
    }
    let mut slots = Vec::new();
    if let Some(autos) = table.get("automations") {
        let autos = autos.as_table().ok_or_else(|| anyhow!("`automations` must be a table"))?;
        for (name, value) in autos {
            let paused = value.get("paused").and_then(|p| p.as_bool()).unwrap_or(false);
            let result = Entry::deserialize(value.clone())
                .map_err(|e| anyhow!("{}", e.message()))
                .and_then(|entry| validate(name, entry));
            slots.push(match result {
                Ok(a) => Slot::Valid(a),
                Err(e) => Slot::Invalid(Invalid {
                    name: name.clone(),
                    error: format!("{e:#}"),
                    kind: str_field(value, "kind"),
                    schedule: str_field(value, "schedule"),
                    paused,
                }),
            });
        }
    }
    Ok(Config { settings, slots })
}

pub fn read_text() -> Result<String> {
    let path = paths::config_file();
    match std::fs::read_to_string(&path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn load() -> Result<Config> {
    parse(&read_text()?)
}

const HEADER: &str = "\
# automate-everything: one [automations.<name>] table per automation.
# Docs: ~/.config/herdr-app/mods/automate-everything/README.md
";

/// Read-modify-write the config under a lock. `edit` changes the document;
/// the result is validated before it's written, atomically.
pub fn edit<F>(name: &str, edit: F) -> Result<()>
where
    F: FnOnce(&mut toml_edit::DocumentMut) -> Result<()>,
{
    paths::ensure_state_dirs()?;
    let _lock = FileLock::acquire(&paths::config_lock())?;
    let path = paths::config_file();
    let mut text = read_text()?;
    if text.trim().is_empty() {
        text = HEADER.to_string();
    }
    let mut doc: toml_edit::DocumentMut = text.parse().context("config is not valid TOML")?;
    edit(&mut doc)?;
    let out = doc.to_string();
    let parsed = parse(&out)?;
    if let Some(Slot::Invalid(i)) = parsed.find(name) {
        bail!("{}: {}", i.name, i.error);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn automations_table(doc: &mut toml_edit::DocumentMut) -> Result<&mut toml_edit::Table> {
    if !doc.contains_key("automations") {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        doc.insert("automations", toml_edit::Item::Table(t));
    }
    doc["automations"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("`automations` must be a table"))
}

fn entry_table<'a>(doc: &'a mut toml_edit::DocumentMut, name: &str) -> Result<&'a mut toml_edit::Table> {
    automations_table(doc)?
        .get_mut(name)
        .and_then(|i| i.as_table_mut())
        .ok_or_else(|| anyhow!("no automation named `{name}`"))
}

pub fn set_paused(name: &str, paused: bool) -> Result<()> {
    edit(name, |doc| {
        let t = entry_table(doc, name)?;
        if paused {
            t["paused"] = toml_edit::value(true);
        } else {
            t.remove("paused");
        }
        Ok(())
    })
}

pub fn remove(name: &str) -> Result<()> {
    edit(name, |doc| {
        automations_table(doc)?
            .remove(name)
            .ok_or_else(|| anyhow!("no automation named `{name}`"))?;
        Ok(())
    })
}

fn json_to_toml(v: &serde_json::Value) -> Result<toml_edit::Item> {
    use serde_json::Value as J;
    Ok(match v {
        J::Bool(b) => toml_edit::value(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml_edit::value(i)
            } else {
                toml_edit::value(n.as_f64().unwrap_or(0.0))
            }
        }
        J::String(s) => toml_edit::value(s.clone()),
        J::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                match json_to_toml(x)? {
                    toml_edit::Item::Value(v) => arr.push(v),
                    _ => bail!("arrays may hold plain values only"),
                }
            }
            toml_edit::value(arr)
        }
        J::Object(o) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, x) in o {
                match json_to_toml(x)? {
                    toml_edit::Item::Value(v) => {
                        t.insert(k, v);
                    }
                    _ => bail!("nested tables are not supported"),
                }
            }
            toml_edit::value(t)
        }
        J::Null => toml_edit::Item::None,
    })
}

/// Creates or updates one entry from JSON fields. `null` removes a field;
/// fields not given are left as they are. With `create`, the entry must not exist.
pub fn upsert(name: &str, fields: &serde_json::Map<String, serde_json::Value>, create: bool) -> Result<()> {
    if !valid_name(name) {
        bail!("name must be letters, digits, `-`, `_` or `.`");
    }
    edit(name, |doc| {
        let autos = automations_table(doc)?;
        let exists = autos.contains_key(name);
        if create && exists {
            bail!("an automation named `{name}` already exists");
        }
        if !create && !exists {
            bail!("no automation named `{name}`");
        }
        if !exists {
            autos.insert(name, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        let t = autos[name].as_table_mut().ok_or_else(|| anyhow!("`{name}` is not a table"))?;
        let mut fields: Vec<_> = fields.iter().collect();
        let rank = |k: &str| FIELD_ORDER.iter().position(|f| *f == k).unwrap_or(FIELD_ORDER.len());
        fields.sort_by_key(|(k, _)| rank(k));
        for (k, v) in fields {
            let item = json_to_toml(v)?;
            if item.is_none() {
                t.remove(k);
            } else if let (Some(old), toml_edit::Item::Value(new)) = (t.get_mut(k), &item) {
                // Keep the field's surrounding comments and spacing.
                if let Some(oldv) = old.as_value_mut() {
                    let decor = oldv.decor().clone();
                    *oldv = new.clone();
                    *oldv.decor_mut() = decor;
                } else {
                    *old = item;
                }
            } else {
                t.insert(k, item);
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_entry_does_not_hide_others() {
        let c = parse(
            r#"
[automations.good]
kind = "script"
schedule = "*/2 * * * *"
command = "true"

[automations.bad]
kind = "script"
schedule = "nope"
command = "true"

[automations.typo]
kind = "script"
schedule = "* * * * *"
comand = "true"
"#,
        )
        .unwrap();
        assert!(matches!(c.find("good"), Some(Slot::Valid(_))));
        assert!(matches!(c.find("bad"), Some(Slot::Invalid(_))));
        match c.find("typo") {
            Some(Slot::Invalid(i)) => assert!(i.error.contains("comand"), "{}", i.error),
            _ => panic!(),
        }
    }

    #[test]
    fn script_rejects_agent_fields() {
        let c = parse("[automations.x]\nkind='script'\nschedule='* * * * *'\ncommand='true'\nprompt='hi'\n").unwrap();
        assert!(matches!(c.find("x"), Some(Slot::Invalid(_))));
    }
}
