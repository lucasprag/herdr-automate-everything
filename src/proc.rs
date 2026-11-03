//! Running programs with a timeout. Each program leads its own process group,
//! so a timeout stops it and everything it started.

use std::fs::File;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct Outcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
}

pub struct Captured {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
}

fn kill_group(child: &Child, sig: i32) {
    let pid = child.id() as i32;
    unsafe {
        libc::kill(-pid, sig);
    }
}

/// Waits for `child`, stopping its group at `timeout` (TERM, then KILL 5 s later).
pub fn wait_with_timeout(child: &mut Child, timeout: Duration) -> std::io::Result<(ExitStatus, bool)> {
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut term_sent_at: Option<Instant> = None;
    loop {
        if let Some(status) = child.try_wait()? {
            if timed_out {
                // Make sure nothing it started outlives it.
                kill_group(child, libc::SIGKILL);
            }
            return Ok((status, timed_out));
        }
        let now = Instant::now();
        match term_sent_at {
            None if now >= deadline => {
                timed_out = true;
                kill_group(child, libc::SIGTERM);
                term_sent_at = Some(now);
            }
            Some(t) if now >= t + Duration::from_secs(5) => {
                kill_group(child, libc::SIGKILL);
                let _ = child.kill();
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn split_status(status: ExitStatus) -> (Option<i32>, Option<i32>) {
    (status.code(), status.signal())
}

/// Runs to completion with stdout and stderr going to files.
pub fn run_to_files(mut cmd: Command, stdout: &Path, stderr: &Path, timeout: Duration) -> std::io::Result<Outcome> {
    cmd.stdin(Stdio::null())
        .stdout(File::create(stdout)?)
        .stderr(File::create(stderr)?)
        .process_group(0);
    let mut child = cmd.spawn()?;
    let (status, timed_out) = wait_with_timeout(&mut child, timeout)?;
    let (exit_code, signal) = split_status(status);
    Ok(Outcome { exit_code, signal, timed_out })
}

fn reader<R: Read + Send + 'static>(mut r: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// Runs to completion and captures its output in memory.
pub fn output_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<Captured> {
    let start = Instant::now();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    if cmd.get_program().is_empty() {
        return Err(std::io::Error::other("empty command"));
    }
    let mut child = cmd.spawn()?;
    let out = reader(child.stdout.take().unwrap());
    let err = reader(child.stderr.take().unwrap());
    let (status, timed_out) = wait_with_timeout(&mut child, timeout)?;
    // Something it left behind may still hold the pipes; don't wait forever.
    let grace = Duration::from_secs(2);
    let stdout = out.recv_timeout(grace).unwrap_or_default();
    let stderr = err.recv_timeout(grace).unwrap_or_default();
    let (exit_code, signal) = split_status(status);
    Ok(Captured { stdout, stderr, exit_code, signal, timed_out, duration: start.elapsed() })
}

/// Keeps the last `max` bytes, cut at a character boundary.
pub fn tail_string(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max {
        return s.into_owned();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("[…{} bytes cut]\n{}", start, &s[start..])
}
