//! Run history: one folder per fire or run under `runs/<id>/` holding
//! `run.json` and, for scripts, `stdout.log` and `stderr.log`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Settings;
use crate::lock;
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Succeeded,
    Failed,
    Blocked,
    SkippedPrecheck,
    SkippedOverlap,
    Missed,
    /// The user closed the run's space or tab before it finished. Not a failure.
    Closed,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Succeeded => "succeeded",
            Status::Failed => "failed",
            Status::Blocked => "blocked",
            Status::SkippedPrecheck => "skipped_precheck",
            Status::SkippedOverlap => "skipped_overlap",
            Status::Missed => "missed",
            Status::Closed => "closed",
        }
    }
    /// Still going: its runner holds the automation's lock.
    pub fn is_live(&self) -> bool {
        matches!(self, Status::Running | Status::Blocked)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Scheduled,
    Manual,
}

impl Trigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            Trigger::Scheduled => "scheduled",
            Trigger::Manual => "manual",
        }
    }
}

/// A command's result: the precheck's, kept whole in the record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub command: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

/// What an agent run opened, so it can close exactly that.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent: String,
    pub cwd: String,
    pub repo: String,
    pub space_id: Option<String>,
    pub space_name: Option<String>,
    /// The run made this space (and so may close it).
    pub space_created: bool,
    pub tab_id: Option<String>,
    pub terminal: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub base_commit: Option<String>,
    pub prompt_accepted: bool,
    #[serde(default)]
    pub was_blocked: bool,
    pub closed: Option<String>,
    pub worktree_cleanup: Option<String>,
    pub output_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub automation: String,
    pub kind: String,
    pub trigger: Trigger,
    pub status: Status,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    pub note: Option<String>,
    /// For `missed`: how many fires were missed, the first one, the last one is `scheduled_for`.
    pub missed_fires: Option<u32>,
    pub first_missed: Option<DateTime<Utc>>,
    /// For a scheduled run that stood in for earlier fires missed while asleep.
    pub caught_up: Option<u32>,
    pub precheck: Option<StepResult>,
    pub runner_pid: Option<u32>,
    pub agent: Option<AgentInfo>,
}

impl Run {
    pub fn new(id: String, automation: &str, kind: &str, trigger: Trigger, status: Status) -> Run {
        Run {
            id,
            automation: automation.to_string(),
            kind: kind.to_string(),
            trigger,
            status,
            scheduled_for: None,
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            duration_ms: None,
            exit_code: None,
            signal: None,
            timed_out: false,
            note: None,
            missed_fires: None,
            first_missed: None,
            caught_up: None,
            precheck: None,
            runner_pid: None,
            agent: None,
        }
    }

    pub fn finish(&mut self, status: Status) {
        let now = Utc::now();
        self.status = status;
        self.finished_at = Some(now);
        if let Some(s) = self.started_at {
            self.duration_ms = Some((now - s).num_milliseconds().max(0) as u64);
        }
    }

    pub fn dir(&self) -> PathBuf {
        run_dir(&self.id)
    }

    pub fn save(&self) -> Result<()> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".run.json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, dir.join("run.json"))?;
        Ok(())
    }
}

pub fn new_id(automation: &str) -> String {
    let mut bytes = [0u8; 2];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    format!(
        "{}-{}-{:02x}{:02x}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        automation,
        bytes[0],
        bytes[1]
    )
}

pub fn run_dir(id: &str) -> PathBuf {
    paths::runs_dir().join(id)
}

pub fn load(id: &str) -> Result<Run> {
    let path = run_dir(id).join("run.json");
    let text = std::fs::read_to_string(&path).with_context(|| format!("no run `{id}`"))?;
    serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))
}

/// A live record whose runner is gone (killed, crashed, machine restarted)
/// is closed as failed.
fn settle_orphan(mut run: Run) -> Run {
    if run.status.is_live() && !lock::is_held(&paths::automation_lock(&run.automation)) {
        run.note = Some(match run.note.take() {
            Some(n) => format!("{n}; runner stopped before recording a result"),
            None => "runner stopped before recording a result".into(),
        });
        run.finish(Status::Failed);
        let _ = run.save();
    }
    run
}

/// Every run, newest first.
pub fn all() -> Vec<Run> {
    let mut runs = Vec::new();
    let Ok(entries) = std::fs::read_dir(paths::runs_dir()) else {
        return runs;
    };
    for e in entries.flatten() {
        let path = e.path().join("run.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(run) = serde_json::from_str::<Run>(&text) {
                runs.push(settle_orphan(run));
            }
        }
    }
    runs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    runs
}

/// The newest run per automation.
pub fn latest_by_automation(runs: &[Run]) -> HashMap<String, Run> {
    let mut m = HashMap::new();
    for r in runs {
        m.entry(r.automation.clone()).or_insert_with(|| r.clone());
    }
    m
}

pub fn delete_for(automation: &str) -> usize {
    let mut n = 0;
    for r in all() {
        if r.automation == automation && std::fs::remove_dir_all(r.dir()).is_ok() {
            n += 1;
        }
    }
    n
}

/// Ages out records older than the retention window and beyond the cap per
/// automation. Live runs are never removed.
pub fn prune(settings: &Settings) -> usize {
    let cutoff = Utc::now() - Duration::days(settings.retention_days as i64);
    let mut kept: HashMap<String, usize> = HashMap::new();
    let mut removed = 0;
    for r in all() {
        if r.status.is_live() {
            continue;
        }
        let n = kept.entry(r.automation.clone()).or_insert(0);
        if r.created_at < cutoff || *n >= settings.max_runs_per_automation {
            if std::fs::remove_dir_all(r.dir()).is_ok() {
                removed += 1;
            }
        } else {
            *n += 1;
        }
    }
    removed
}
