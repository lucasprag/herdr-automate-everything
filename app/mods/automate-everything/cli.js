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

export const list = () => json(["list", "--json"]);
export const history = (limit = 200) => json(["history", "--json", "--limit", String(limit)]);
export const show = (id) => json(["show", id, "--json"]);
export const get = (name) => json(["get", name]);
export const status = () => json(["status", "--json"]);
export const runNow = (name) => json(["run", name, "--json"]);
export const pause = (name) => call(["pause", name]);
export const resume = (name) => call(["resume", name]);
export const remove = (name) => call(["delete", name, "--yes"]);
export const move = (name, to) => call(["move", name, "--to", String(to)]);
export const save = (name, fields, create) =>
  call(["set", name, "--fields", JSON.stringify(fields), ...(create ? ["--create"] : [])]);
