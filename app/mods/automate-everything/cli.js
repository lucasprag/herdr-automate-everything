// The panel's only way in: the automate-everything CLI. It owns the config
// file, history and runs; the panel never writes them itself.

import { exec, home } from "herdr";
import { exists } from "fs/promises";

let bin = null;

// Installed by install.sh (service ensure) in ~/.local/bin.
async function binary() {
  if (bin) return bin;
  const c = `${home()}/.local/bin/automate-everything`;
  try {
    if (await exists(c)) return (bin = c);
  } catch {}
  throw new Error("automate-everything isn't installed yet: the panel runs install.sh, see mods/automate-everything/.target/install.log");
}

async function call(args) {
  const r = await exec(await binary(), args);
  if (r.code !== 0) {
    const msg = (r.stderr || r.stdout || `exit ${r.code}`).trim().replace(/^Error:\s*/, "");
    throw new Error(msg);
  }
  return r.stdout;
}

const json = async (args) => JSON.parse(await call(args));

// `on`: a machine whose own scheduler holds the entry (`--on <machine>`); the
// CLI answers from there, failing fast when it's offline. null: this Mac.
const at = (on, args) => (on ? ["--on", on, ...args] : args);

export const list = (on) => json(at(on, ["list", "--json"]));
export const history = (limit = 200, on) => json(at(on, ["history", "--json", "--limit", String(limit)]));
export const show = (id, on) => json(at(on, ["show", id, "--json"]));
export const get = (name, on) => json(at(on, ["get", name]));
export const status = (on) => json(at(on, ["status", "--json"]));
// With `probe`, each connected machine says whether it runs automate-everything.
export const machines = (probe = false) => json(["machines", "--json", ...(probe ? ["--probe"] : [])]);
export const runNow = (name, on) => json(at(on, ["run", name, "--json"]));
export const pause = (name, on) => call(at(on, ["pause", name]));
export const resume = (name, on) => call(at(on, ["resume", name]));
export const remove = (name, on) => call(at(on, ["delete", name, "--yes"]));
export const move = (name, to, on) => call(at(on, ["move", name, "--to", String(to)]));
export const save = (name, fields, create, on) =>
  call(at(on, ["set", name, "--fields", JSON.stringify(fields), ...(create ? ["--create"] : [])]));
// Hands the entry from `on` (null: this Mac) to `to` (a machine, or "here").
export const transfer = (name, to, fields, on) =>
  call(at(on, ["transfer", name, "--to", to, ...(fields ? ["--fields", JSON.stringify(fields)] : [])]));

// Machines that can schedule entries: running automate-everything new enough
// for `--on`.
export const PROTOCOL = 1;
export const schedulers = (ms) => ms.filter((m) => m.automate_everything?.installed && (m.automate_everything.protocol ?? 0) >= PROTOCOL);
