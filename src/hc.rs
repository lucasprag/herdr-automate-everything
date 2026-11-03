//! herdr-core (`hc`): terminals, and the `herdr-app` document where the app
//! keeps its spaces and tabs. Everything here works with the app window
//! closed: a terminal carrying `herdr.space`/`herdr.tab` labels shows up as
//! that space and tab, and names live in the document.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::{env, paths, proc};

pub const DOC: &str = "herdr-app";

fn find_in_path(name: &str) -> Option<PathBuf> {
    let env = env::login_env();
    let path = env.get("PATH")?;
    path.split(':').map(|d| PathBuf::from(d).join(name)).find(|p| p.is_file())
}

pub fn bin() -> PathBuf {
    find_in_path("hc").unwrap_or_else(|| paths::home().join(".local/bin/hc"))
}

fn command() -> Command {
    let mut c = Command::new(bin());
    // Never run as a child of whatever terminal started us.
    c.env_remove("HC_TERMINAL_ID").stdin(Stdio::null());
    c
}

fn run_json(args: &[&str], timeout: Duration) -> Result<(Option<i32>, Value, String)> {
    let mut c = command();
    c.args(args);
    let out = proc::output_with_timeout(c, timeout).context("running hc")?;
    if out.timed_out {
        bail!("hc {} timed out", args.first().unwrap_or(&""));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let v = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    Ok((out.exit_code, v, stderr))
}

/// One terminal's facts, live or finished; None when the core doesn't know it.
pub fn get(addr: &str) -> Result<Option<Value>> {
    let (code, v, err) = run_json(&["get", addr, "--json"], Duration::from_secs(20))?;
    match code {
        Some(0) => Ok(Some(v)),
        Some(3) => Ok(None),
        _ => bail!("hc get {addr}: {}", err.trim()),
    }
}

/// Live terminals carrying every label given.
pub fn live_with_labels(labels: &[(&str, &str)]) -> Result<Vec<Value>> {
    let mut args = vec!["ls".to_string(), "--json".to_string()];
    for (k, v) in labels {
        args.push("--label".into());
        args.push(format!("{k}={v}"));
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let (code, v, err) = run_json(&refs, Duration::from_secs(20))?;
    if code != Some(0) {
        bail!("hc ls: {}", err.trim());
    }
    Ok(v["terminals"].as_array().cloned().unwrap_or_default())
}

pub struct Launch<'a> {
    pub cwd: &'a str,
    pub labels: Vec<(String, String)>,
    pub argv: Vec<String>,
}

/// Starts a kept terminal that belongs to no one, so it outlives us.
pub fn launch(l: &Launch) -> Result<String> {
    let mut args: Vec<String> = ["run", "--bg", "--json", "--keep", "--no-parent", "--outlive-parent", "--no-notify"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push("--cwd".into());
    args.push(l.cwd.into());
    for (k, v) in &l.labels {
        args.push("--label".into());
        args.push(format!("{k}={v}"));
    }
    args.push("--".into());
    args.extend(l.argv.iter().cloned());
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let (code, v, err) = run_json(&refs, Duration::from_secs(60))?;
    if code != Some(0) {
        bail!("hc run failed: {}", err.trim());
    }
    v["address"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| anyhow!("hc run gave no address: {v}"))
}

/// The screen plus history, as text.
pub fn read(addr: &str, lines: usize) -> Result<String> {
    let mut c = command();
    c.args(["read", addr, "--lines", &lines.to_string()]);
    let out = proc::output_with_timeout(c, Duration::from_secs(30))?;
    if out.exit_code != Some(0) {
        bail!("hc read {addr}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Presses named keys (`down`, `enter`) once nobody has typed for a moment.
pub fn keys(addr: &str, keys: &[&str]) -> Result<()> {
    let mut args = vec!["keys", addr];
    args.extend_from_slice(keys);
    args.push("--json");
    let (code, _, err) = run_json(&args, Duration::from_secs(30))?;
    if code != Some(0) {
        bail!("hc keys {addr}: {}", err.trim());
    }
    Ok(())
}

pub fn kill(addr: &str) -> Result<()> {
    let (code, _, err) = run_json(&["kill", addr, "--json"], Duration::from_secs(30))?;
    if code != Some(0) && code != Some(11) {
        bail!("hc kill {addr}: {}", err.trim());
    }
    Ok(())
}

/// One API call over `hc connect`.
pub fn rpc(method: &str, params: Value) -> Result<Value> {
    let mut c = command();
    c.arg("connect").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().context("starting hc connect")?;
    {
        let mut stdin = child.stdin.take().unwrap();
        let hello = json!({"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {}});
        let call = json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": params});
        writeln!(stdin, "{hello}\n{call}")?;
    }
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v["id"] == 2 {
            if let Some(e) = v.get("error") {
                let kind = e["data"]["kind"].as_str().unwrap_or("");
                bail!("{method}: {} {}", kind, e["message"].as_str().unwrap_or(""));
            }
            return Ok(v["result"].clone());
        }
    }
    bail!("{method}: no answer from hc ({})", String::from_utf8_lossy(&out.stderr).trim())
}

pub fn doc_get() -> Result<(Value, u64)> {
    let r = rpc("doc.get", json!({ "name": DOC }))?;
    Ok((r["value"].clone(), r["version"].as_u64().unwrap_or(0)))
}

/// Changes the app's document, retrying when someone else wrote it first.
/// `f` returns false to leave it unchanged.
pub fn doc_update<F>(mut f: F) -> Result<()>
where
    F: FnMut(&mut Value) -> Result<bool>,
{
    for _ in 0..8 {
        let (mut doc, version) = doc_get()?;
        if doc.is_null() {
            doc = json!({ "spaces": [] });
        }
        if !f(&mut doc)? {
            return Ok(());
        }
        match rpc("doc.set", json!({ "name": DOC, "value": doc, "version": version })) {
            Ok(_) => return Ok(()),
            Err(e) if format!("{e}").contains("conflict") => {
                std::thread::sleep(Duration::from_millis(300));
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    bail!("the app kept changing its document; gave up")
}

pub fn new_id(prefix: &str) -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}{hex}")
}

pub fn spaces(doc: &Value) -> Vec<Value> {
    doc["spaces"].as_array().cloned().unwrap_or_default()
}

/// An open space on this machine with this name.
pub fn find_space_by_name(name: &str) -> Result<Option<String>> {
    let (doc, _) = doc_get()?;
    Ok(spaces(&doc)
        .iter()
        .find(|s| s["name"].as_str() == Some(name) && s.get("machine").map(|m| m.is_null()).unwrap_or(true))
        .and_then(|s| s["id"].as_str().map(String::from)))
}

/// Names a space and one of its tabs, adding them to the document when the
/// app hasn't (it isn't running).
pub fn name_space_and_tab(
    space_id: &str,
    space_name: &str,
    folder: &str,
    tab_id: &str,
    tab_name: &str,
    terminal: &str,
) -> Result<()> {
    doc_update(|doc| {
        if !doc["spaces"].is_array() {
            doc["spaces"] = json!([]);
        }
        let list = doc["spaces"].as_array_mut().unwrap();
        let space = match list.iter_mut().position(|s| s["id"] == space_id) {
            Some(i) => &mut list[i],
            None => {
                list.push(json!({ "id": space_id, "name": space_name, "folder": folder, "tabs": [] }));
                list.last_mut().unwrap()
            }
        };
        let mut changed = false;
        if space["name"].as_str() != Some(space_name) {
            space["name"] = json!(space_name);
            changed = true;
        }
        if !space["tabs"].is_array() {
            space["tabs"] = json!([]);
        }
        let tabs = space["tabs"].as_array_mut().unwrap();
        match tabs.iter_mut().find(|t| t["id"] == tab_id) {
            Some(t) => {
                if t["name"].as_str() != Some(tab_name) {
                    t["name"] = json!(tab_name);
                    changed = true;
                }
            }
            None => {
                tabs.push(json!({
                    "id": tab_id,
                    "name": tab_name,
                    "root": { "type": "row", "id": tab_id, "children": [
                        { "type": "block", "id": new_id("p"), "content": { "terminal": terminal } }
                    ] }
                }));
                changed = true;
            }
        }
        Ok(changed)
    })
}

/// Drops a tab (and its space, when it was the last) from the document,
/// for when the app isn't running to notice its terminals ended.
pub fn forget_tab(space_id: &str, tab_id: &str) -> Result<()> {
    doc_update(|doc| {
        let Some(list) = doc["spaces"].as_array_mut() else { return Ok(false) };
        let Some(i) = list.iter().position(|s| s["id"] == space_id) else { return Ok(false) };
        let Some(tabs) = list[i]["tabs"].as_array_mut() else { return Ok(false) };
        let before = tabs.len();
        tabs.retain(|t| t["id"] != tab_id);
        if tabs.len() == before {
            return Ok(false);
        }
        if tabs.is_empty() {
            list.remove(i);
        }
        Ok(true)
    })
}
