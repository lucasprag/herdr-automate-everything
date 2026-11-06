//! The scheduler: one process (a launchd agent), held to one by a kernel lock.
//! Every minute it finds the fires each automation had since it last looked,
//! starts a run for the newest one if it's inside the grace window, and
//! records `missed` otherwise. Runs are separate processes, so restarting the
//! scheduler (a new binary) never stops them.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::config::{self, Slot};
use crate::history::{self, Run, Status, Trigger};
use crate::lock::FileLock;
use crate::paths;
use crate::runner::{self, Request};

const MAX_LOOKBACK_DAYS: i64 = 40;
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

pub fn log(msg: &str) {
    eprintln!("{} {msg}", Utc::now().format("%Y-%m-%dT%H:%M:%SZ"));
}

type Cursors = BTreeMap<String, DateTime<Utc>>;

fn load_cursors() -> Cursors {
    std::fs::read_to_string(paths::cursors_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_cursors(c: &Cursors) {
    let path = paths::cursors_file();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(c).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn exe_identity() -> Option<(u64, i64)> {
    let exe = std::env::current_exe().ok()?;
    let m = std::fs::metadata(exe).ok()?;
    Some((m.ino(), m.mtime()))
}

struct State {
    cursors: Cursors,
    reported: HashMap<String, String>,
    config_error: Option<String>,
}

fn tick(st: &mut State) {
    let now = Utc::now();
    let cfg = match config::load() {
        Ok(c) => {
            if st.config_error.take().is_some() {
                log("config readable again");
            }
            c
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if st.config_error.as_deref() != Some(&msg) {
                log(&format!("config unreadable, nothing fires until it's fixed: {msg}"));
                st.config_error = Some(msg);
            }
            return;
        }
    };

    // Report each invalid entry once per distinct error; the others keep running.
    for slot in &cfg.slots {
        if let Slot::Invalid(i) = slot {
            if st.reported.get(&i.name) != Some(&i.error) {
                log(&format!("skipping invalid entry `{}`: {}", i.name, i.error));
                st.reported.insert(i.name.clone(), i.error.clone());
            }
        } else {
            st.reported.remove(slot.name());
        }
    }

    for a in cfg.valid() {
        let cursor = *st.cursors.entry(a.name.clone()).or_insert(now);
        st.cursors.insert(a.name.clone(), now);
        if a.paused() {
            continue;
        }
        let from = cursor.max(now - Duration::days(MAX_LOOKBACK_DAYS));
        let fires = a.cron.fires_between(&a.tz, from, now, usize::MAX);
        let Some(&latest) = fires.last() else { continue };
        let grace = Duration::minutes(a.grace_minutes() as i64) + Duration::seconds(59);
        if now - latest <= grace {
            let req = Request {
                name: a.name.clone(),
                trigger: Trigger::Scheduled,
                run_id: history::new_id(&a.name),
                scheduled_for: Some(latest),
                caught_up: Some((fires.len() - 1) as u32),
            };
            match runner::spawn(&req) {
                Ok(mut child) => {
                    log(&format!("fired {} ({})", a.name, req.run_id));
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                Err(e) => {
                    log(&format!("could not start {}: {e:#}", a.name));
                    let mut run = Run::new(req.run_id, &a.name, a.kind.as_str(), Trigger::Scheduled, Status::Failed);
                    run.scheduled_for = Some(latest);
                    run.note = Some(format!("could not start the runner: {e:#}"));
                    run.finish(Status::Failed);
                    let _ = run.save();
                }
            }
        } else {
            let mut run = Run::new(history::new_id(&a.name), &a.name, a.kind.as_str(), Trigger::Scheduled, Status::Missed);
            run.scheduled_for = Some(latest);
            run.missed_fires = Some(fires.len() as u32);
            run.first_missed = fires.first().copied();
            run.finished_at = Some(now);
            run.note = Some(format!(
                "{} fire{} older than the {} min grace window",
                fires.len(),
                if fires.len() == 1 { "" } else { "s" },
                a.grace_minutes()
            ));
            let _ = run.save();
            log(&format!("missed {} ({} fires, newest {})", a.name, fires.len(), latest.to_rfc3339()));
        }
    }

    crate::agent::reap_worktrees();

    let names: std::collections::HashSet<&str> = cfg.slots.iter().map(|s| s.name()).collect();
    st.cursors.retain(|k, _| names.contains(k.as_str()));
    save_cursors(&st.cursors);
}

fn trim_log() {
    let path = paths::scheduler_log();
    if let Ok(m) = std::fs::metadata(&path) {
        if m.len() > LOG_MAX_BYTES {
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
                let _ = f.set_len(0);
            }
        }
    }
}

pub fn serve() -> Result<()> {
    paths::ensure_state_dirs()?;
    let lock_path = paths::scheduler_lock();
    let mut lock = match FileLock::try_acquire(&lock_path)? {
        Some(l) => l,
        None => {
            log("another scheduler holds the lock; waiting for it");
            FileLock::acquire(&lock_path)?
        }
    };
    lock.note(&format!("pid {}", std::process::id()));
    log(&format!("scheduler {} started (pid {}, config {})", env!("CARGO_PKG_VERSION"), std::process::id(), paths::config_file().display()));

    let exe = exe_identity();
    let mut st = State { cursors: load_cursors(), reported: HashMap::new(), config_error: None };
    let mut last_prune: Option<DateTime<Utc>> = None;

    loop {
        tick(&mut st);

        if last_prune.map(|t| Utc::now() - t > Duration::hours(1)).unwrap_or(true) {
            if let Ok(cfg) = config::load() {
                let n = history::prune(&cfg.settings);
                if n > 0 {
                    log(&format!("aged out {n} history records"));
                }
            }
            trim_log();
            last_prune = Some(Utc::now());
        }

        // Runs started before an exec (below) are still this pid's children
        // but no thread waits on them any more; reap them here.
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}

        // A new binary takes over in place, keeping the pid. Exiting and
        // leaving the restart to launchd's KeepAlive isn't reliable: launchd
        // can pend that respawn ("inefficient") indefinitely. The lock's fd is
        // close-on-exec, so the new image takes the lock again.
        if exe_identity() != exe {
            log("binary changed; restarting as the new one");
            let err = match std::env::current_exe() {
                Ok(path) => Command::new(path).args(std::env::args_os().skip(1)).exec(),
                Err(e) => e,
            };
            log(&format!("couldn't restart as the new binary ({err}); exiting so launchd starts it"));
            return Ok(());
        }

        // Sleep to the next minute in short steps: the wall clock jumps after
        // the laptop wakes, and a long sleep would notice late.
        let now = Utc::now();
        let next = crate::cron::floor_minute(now) + Duration::minutes(1) + Duration::seconds(1);
        while Utc::now() < next {
            let left = (next - Utc::now()).to_std().unwrap_or_default();
            std::thread::sleep(left.min(StdDuration::from_secs(5)));
        }
    }
}
