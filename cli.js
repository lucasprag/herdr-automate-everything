// The panel's only way in: the automate-everything CLI. It owns the config
// file, history and runs; the panel never writes them itself.

import { exec, home, appHome } from "herdr";
import { exists } from "fs/promises";

let bin = null;

// ~/.local/bin (installed), else this folder's own build.
async function binary() {
  if (bin) return bin;
  const candidates = [
    `${home()}/.local/bin/automate-everything`,
    `${appHome()}/mods/automate-everything/.target/release/automate-everything`,
    `${appHome()}/mods/automate-everything/.target/debug/automate-everything`,
  ];
  for (const c of candidates) {
    try {
      // Remember only the installed one; until it exists, look again each time.
      if (await exists(c)) return c === candidates[0] ? (bin = c) : c;
    } catch {}
  }
  throw new Error("automate-everything isn't installed: run install.sh in mods/automate-everything");
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
export const save = (name, fields, create) =>
  call(["set", name, "--fields", JSON.stringify(fields), ...(create ? ["--create"] : [])]);
