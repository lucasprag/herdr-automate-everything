//! Where a run executes: this Mac, or another herdr-core machine (an
//! automation's `machine`). Paths, files, git and signals go through here, so
//! for a machine they resolve there: `~` is that machine's home, and commands
//! get that machine's login environment, never this Mac's.
//!
//! On a machine, a command runs in a terminal there (`hc run --on`), not
//! `hc exec`: exec caps a run at 600 s and ends anything it started when it
//! returns. The terminal writes stdout and stderr to files on the machine,
//! which are copied back when it ends; a timeout ends it with `hc kill`, which
//! ends everything in its session there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::{env, hc, paths, proc};

pub enum Host {
    Local,
    Remote(Remote),
}

pub struct Remote {
    pub name: String,
    pub home: PathBuf,
    /// Its login shell (`$SHELL` there).
    pub shell: String,
}

/// Where a run's output waits on the machine until it's copied back.
const REMOTE_RUNS: &str = ".cache/automate-everything/runs";
/// The most of each stream copied back.
const FETCH_MAX: usize = 4 * 1024 * 1024;
const POLL: Duration = Duration::from_secs(1);
/// Printed before a remote git's own output, so whatever a login profile
/// prints first can be told apart from it.
const GIT_MARKER: &str = "\u{0}__AUTOMATE_EVERYTHING_GIT__\u{0}";
/// How long hc may fail to answer about a running command before giving up.
const LOST_AFTER: Duration = Duration::from_secs(120);
/// How long a command stopped at its timeout may take to end before giving up
/// on it (it may still be running there).
const KILL_WAIT: Duration = Duration::from_secs(60);

/// Runs a /bin/sh script inside the machine's login shell, whatever that
/// shell is: the script goes in `$AE_SCRIPT`, and the only thing the login
/// shell is asked to do, `exec /bin/sh -c "$AE_SCRIPT"`, reads the same in
/// sh, bash, zsh and fish. Arguments: output folder, script.
const OUTER: &str = r#"mkdir -p "$1" || exit 125
AE_SCRIPT=$2; export AE_SCRIPT
exec </dev/null
exec "${SHELL:-/bin/sh}" -lic 'exec /bin/sh -c "$AE_SCRIPT"'"#;
/// The same for a short helper in a non-interactive login shell. Arguments:
/// the login shell, script.
const LOGIN: &str = r#"AE_SCRIPT=$2; export AE_SCRIPT
exec "$1" -lc 'exec /bin/sh -c "$AE_SCRIPT"'"#;

/// `s` as one /bin/sh word.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r#"'\''"#))
}

/// After the login shell: the entry's env on top, output to files in `dir`,
/// `cwd`, then the command.
fn inner_script(dir: &Path, cwd: &Path, command: &str, vars: &BTreeMap<String, String>) -> String {
    let d = |name: &str| sh_quote(&dir.join(name).display().to_string());
    let mut s = String::from("unset AE_SCRIPT\n");
    for (k, v) in vars {
        s.push_str(&format!("export {k}={}\n", sh_quote(v)));
    }
    s.push_str(&format!("exec >{} 2>{} </dev/null\n", d("stdout.log"), d("stderr.log")));
    s.push_str(&format!("cd {} || exit 126\n", sh_quote(&cwd.display().to_string())));
    s.push_str(&format!("exec /bin/sh -c {}", sh_quote(command)));
    s
}

impl Host {
    /// This Mac for None; for a machine, asks it for its home and shell,
    /// which is also the check that it can be reached now.
    pub fn connect(machine: Option<&str>) -> Result<Host> {
        let Some(m) = machine else { return Ok(Host::Local) };
        let probe = hc::exec_on(m, &["/bin/sh", "-c", r#"printf '%s\n%s\n' "$HOME" "${SHELL:-/bin/sh}""#], 30)
            .and_then(|o| if o.ok() { Ok(o) } else { bail!("{}", o.stderr_text()) })
            .map_err(|e| anyhow!("machine `{m}` unreachable: {e:#}"))?;
        let text = probe.stdout_text();
        let mut lines = text.lines();
        let home = lines.next().filter(|h| h.starts_with('/')).ok_or_else(|| anyhow!("machine `{m}`: no home folder"))?;
        let shell = lines.next().filter(|s| !s.is_empty()).unwrap_or("/bin/sh");
        Ok(Host::Remote(Remote { name: m.to_string(), home: PathBuf::from(home), shell: shell.to_string() }))
    }

    pub fn machine(&self) -> Option<&str> {
        match self {
            Host::Local => None,
            Host::Remote(r) => Some(&r.name),
        }
    }

    pub fn home(&self) -> PathBuf {
        match self {
            Host::Local => paths::home(),
            Host::Remote(r) => r.home.clone(),
        }
    }

    pub fn expand(&self, p: &str) -> PathBuf {
        paths::expand_in(&self.home(), p)
    }

    /// The login shell the agent starts in.
    pub fn shell(&self) -> String {
        match self {
            Host::Local => env::shell_of(&env::login_env()),
            Host::Remote(r) => r.shell.clone(),
        }
    }

    pub fn read_to_string(&self, path: &Path) -> Result<String> {
        match self {
            Host::Local => std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display())),
            Host::Remote(r) => {
                let o = r.exec(&["cat", "--", &path.display().to_string()], 30)?;
                if !o.ok() {
                    bail!("reading {} on `{}`: {}", path.display(), r.name, o.stderr_text());
                }
                if o.truncated {
                    bail!("reading {} on `{}`: too big for hc to copy back whole", path.display(), r.name);
                }
                Ok(o.stdout_text())
            }
        }
    }

    /// Whether `path` exists; true when a machine can't be asked, so cleanup
    /// still tries.
    pub fn exists(&self, path: &Path) -> bool {
        match self {
            Host::Local => path.exists(),
            Host::Remote(r) => r.test("-e", path).unwrap_or(true),
        }
    }

    /// None when a machine can't be asked.
    pub fn is_dir(&self, path: &Path) -> Option<bool> {
        match self {
            Host::Local => Some(path.is_dir()),
            Host::Remote(r) => r.test("-d", path),
        }
    }

    /// None when a machine can't be asked.
    pub fn is_file(&self, path: &Path) -> Option<bool> {
        match self {
            Host::Local => Some(path.is_file()),
            Host::Remote(r) => r.test("-f", path),
        }
    }

    pub fn create_dir_all(&self, path: &Path) -> Result<()> {
        match self {
            Host::Local => Ok(std::fs::create_dir_all(path)?),
            Host::Remote(r) => {
                let o = r.exec(&["mkdir", "-p", "--", &path.display().to_string()], 30)?;
                if !o.ok() {
                    bail!("mkdir {} on `{}`: {}", path.display(), r.name, o.stderr_text());
                }
                Ok(())
            }
        }
    }

    /// Removes `from` and its parents while they're empty, stopping at `root`.
    pub fn remove_empty_dirs(&self, from: &Path, root: &Path) {
        match self {
            Host::Local => {
                let mut dir = Some(from.to_path_buf());
                while let Some(d) = dir {
                    if !d.starts_with(root) || d == root || std::fs::remove_dir(&d).is_err() {
                        break;
                    }
                    dir = d.parent().map(Path::to_path_buf);
                }
            }
            Host::Remote(r) => {
                let script = r#"d=$1 r=$2
while [ "$d" != "$r" ]; do
  case $d in "$r"/*) ;; *) break ;; esac
  rmdir -- "$d" 2>/dev/null || break
  d=$(dirname -- "$d")
done"#;
                let (from, root) = (from.display().to_string(), root.display().to_string());
                let _ = r.exec(&["/bin/sh", "-c", script, "sh", &from, &root], 30);
            }
        }
    }

    /// Sends `sig` to one process.
    pub fn signal(&self, pid: i64, sig: i32) {
        match self {
            Host::Local => unsafe {
                libc::kill(pid as i32, sig);
            },
            Host::Remote(r) => {
                let _ = r.exec(&["kill", &format!("-{sig}"), &pid.to_string()], 30);
            }
        }
    }

    /// `git -C <repo> <args>`, its stdout trimmed; errs on a non-zero exit.
    pub fn git(&self, repo: &Path, args: &[&str], timeout: u64) -> Result<String> {
        let (code, timed_out, stdout, stderr) = match self {
            Host::Local => {
                let env = env::login_env();
                let mut cmd = std::process::Command::new("git");
                cmd.arg("-C").arg(repo).args(args).env_clear().envs(&env).stdin(std::process::Stdio::null());
                let o = proc::output_with_timeout(cmd, Duration::from_secs(timeout))?;
                (o.exit_code, o.timed_out, o.stdout, o.stderr)
            }
            Host::Remote(r) => {
                // A login shell for the machine's PATH. Its profile may print
                // things; git's own output starts after the marker.
                let mut git = vec!["git".to_string(), "-C".into(), repo.display().to_string()];
                git.extend(args.iter().map(|a| a.to_string()));
                let words: Vec<String> = git.iter().map(|w| sh_quote(w)).collect();
                let script = format!(
                    "unset AE_SCRIPT; printf '{}'; exec {}",
                    GIT_MARKER.replace('\0', "\\0"),
                    words.join(" ")
                );
                let o = r.exec(&["/bin/sh", "-c", LOGIN, "sh", &r.shell, &script], timeout)?;
                if o.truncated {
                    bail!("git {}: output too big for hc to copy back whole", args.join(" "));
                }
                let stdout = after_marker(&o.stdout, GIT_MARKER.as_bytes()).to_vec();
                (o.exit_code, o.timed_out, stdout, o.stderr)
            }
        };
        if timed_out {
            bail!("git {} timed out", args.join(" "));
        }
        if code != Some(0) {
            bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&stderr).trim());
        }
        Ok(String::from_utf8_lossy(&stdout).trim().to_string())
    }
}

/// A command's result on a machine. `error` is set when hc lost track of it
/// before it ended (its output is whatever could be copied back by then).
pub struct RemoteRun {
    pub out: proc::Captured,
    pub error: Option<String>,
}

impl Remote {
    fn exec(&self, argv: &[&str], timeout: u64) -> Result<hc::ExecOut> {
        hc::exec_on(&self.name, argv, timeout).with_context(|| format!("on `{}`", self.name))
    }

    fn test(&self, flag: &str, path: &Path) -> Option<bool> {
        let path = path.display().to_string();
        self.exec(&["test", flag, &path], 30).ok().map(|o| o.ok())
    }

    /// Runs `command` with `/bin/sh -c` on the machine, in `cwd`, in its login
    /// environment plus `vars`, stopping it at `timeout`. `key` names its
    /// output folder there (unique per run). `labels` go on its terminal.
    /// Errs only when it couldn't be started; whatever happens after, its
    /// output is copied back and its folder and terminal are removed.
    pub fn run_command(
        &self,
        key: &str,
        cwd: &Path,
        command: &str,
        vars: &BTreeMap<String, String>,
        timeout: Duration,
        labels: &[(String, String)],
    ) -> Result<RemoteRun> {
        let start = Instant::now();
        let dir = self.home.join(REMOTE_RUNS).join(key);
        let argv: Vec<String> = vec![
            "/bin/sh".into(),
            "-c".into(),
            OUTER.into(),
            "sh".into(),
            dir.display().to_string(),
            inner_script(&dir, cwd, command, vars),
        ];
        let home = self.home.display().to_string();
        let addr = hc::launch(&hc::Launch {
            on: Some(&self.name),
            cwd: &home,
            labels: labels.to_vec(),
            argv,
            keep: false,
            rm: false,
        })
        .map_err(|e| anyhow!("machine `{}` unreachable: {e:#}", self.name))?;

        let mut timed_out = false;
        let mut killed_at: Option<Instant> = None;
        let mut last_ok = Instant::now();
        let finished: Result<serde_json::Value, String> = loop {
            std::thread::sleep(POLL);
            match hc::get(&addr) {
                Ok(Some(t)) if t["state"] == "finished" => break Ok(t),
                Ok(Some(_)) => last_ok = Instant::now(),
                Ok(None) => break Err(format!("its terminal {addr} on `{}` is gone", self.name)),
                Err(e) if last_ok.elapsed() > LOST_AFTER => {
                    let _ = hc::kill(&addr);
                    let m = &self.name;
                    break Err(format!("lost touch with `{m}` while it ran (it may still be running there): {e:#}"));
                }
                Err(_) => {}
            }
            let now = Instant::now();
            match killed_at {
                None if start.elapsed() >= timeout => {
                    timed_out = true;
                    let _ = hc::kill(&addr);
                    killed_at = Some(now);
                }
                // Stopped, but it won't end: give up on it rather than wait forever.
                Some(_) if start.elapsed() >= timeout + KILL_WAIT => {
                    let m = &self.name;
                    break Err(format!(
                        "stopped at its timeout on `{m}` but it hadn't ended {}s later (it may still be running there)",
                        KILL_WAIT.as_secs()
                    ));
                }
                // `hc kill` escalates on its own; ask again now and then.
                Some(t) if now >= t + Duration::from_secs(10) => {
                    let _ = hc::kill(&addr);
                    killed_at = Some(now);
                }
                _ => {}
            }
        };
        let (exit_code, signal) = match &finished {
            Ok(t) => (t["exit_code"].as_i64().map(|c| c as i32), t["signal"].as_i64().map(|c| c as i32)),
            Err(_) => (None, None),
        };
        let duration = start.elapsed();

        let m = &self.name;
        // The last FETCH_MAX bytes, saying so when there were more (its size
        // comes first, on stderr).
        let fetch = |name: &str| -> Vec<u8> {
            let f = dir.join(name).display().to_string();
            let max = FETCH_MAX.to_string();
            let script = r#"n=$(wc -c < "$1") || exit 1; echo "$n" >&2; exec tail -c "$2" -- "$1""#;
            match self.exec(&["/bin/sh", "-c", script, "sh", &f, &max], 120) {
                Ok(o) if o.ok() => {
                    let size: usize = o.stderr_text().trim().parse().unwrap_or(0);
                    let mut b = Vec::new();
                    if size > FETCH_MAX {
                        b.extend(format!("[the first {} bytes of {name} were left on `{m}`]\n", size - FETCH_MAX).bytes());
                    } else if o.truncated {
                        b.extend(format!("[{name} was cut on the way back from `{m}`]\n").bytes());
                    }
                    b.extend_from_slice(&o.stdout);
                    b
                }
                Ok(o) => format!("[could not copy {name} back from `{m}`: {}]\n", o.stderr_text()).into_bytes(),
                Err(e) => format!("[could not copy {name} back from `{m}`: {e:#}]\n").into_bytes(),
            }
        };
        let stdout = fetch("stdout.log");
        let mut stderr = fetch("stderr.log");
        // 125 is also a command's own exit code: only the missing folder says
        // it couldn't be made.
        if exit_code == Some(125) && self.test("-d", &dir) == Some(false) {
            stderr.extend_from_slice(format!("[could not make {} on `{m}`]\n", dir.display()).as_bytes());
        }
        let _ = self.exec(&["rm", "-rf", "--", &dir.display().to_string()], 60);
        let _ = hc::rm(&addr);
        Ok(RemoteRun {
            out: proc::Captured { stdout, stderr, exit_code, signal, timed_out, duration },
            error: finished.err(),
        })
    }
}

/// What follows the first `marker`; all of it when there's none.
fn after_marker<'a>(out: &'a [u8], marker: &[u8]) -> &'a [u8] {
    match out.windows(marker.len()).position(|w| w == marker) {
        Some(i) => &out[i + marker.len()..],
        None => out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_words_survive_sh() {
        for w in ["plain", "it's", "a b", "$HOME", "`x`", "back\\slash", ""] {
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", &format!("printf %s {}", sh_quote(w))])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), w);
        }
    }

    #[test]
    fn the_inner_script_runs_the_command_with_its_env_in_its_folder() {
        let tmp = std::env::temp_dir().join(format!("ae-inner-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let vars = BTreeMap::from([("GREETING".to_string(), "it's \"hi\"".to_string())]);
        let script = inner_script(&tmp, Path::new("/"), "printf '%s %s' \"$GREETING\" \"$(pwd)\"", &vars);
        let status = std::process::Command::new("/bin/sh").args(["-c", &script]).status().unwrap();
        assert!(status.success());
        let out = std::fs::read_to_string(tmp.join("stdout.log")).unwrap();
        assert_eq!(out, "it's \"hi\" /");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn git_output_starts_after_the_marker() {
        let m = GIT_MARKER.as_bytes();
        let mut out = b"Welcome to lab!\n".to_vec();
        out.extend_from_slice(m);
        out.extend_from_slice(b"abc123\n");
        assert_eq!(after_marker(&out, m), b"abc123\n");
        assert_eq!(after_marker(b"abc123\n", m), b"abc123\n");
    }
}
