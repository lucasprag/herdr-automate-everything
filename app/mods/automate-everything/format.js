// Times and status words, shared by the panel and its popups.

export function ago(iso) {
  if (!iso) return "";
  const s = Math.round((Date.now() - Date.parse(iso)) / 1000);
  const a = Math.abs(s);
  const n = a < 60 ? `${a}s` : a < 3600 ? `${Math.round(a / 60)}m` : a < 86400 ? `${Math.round(a / 3600)}h` : `${Math.round(a / 86400)}d`;
  return s < 0 ? `in ${n}` : `${n} ago`;
}

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const pad = (n) => String(n).padStart(2, "0");

// "07:30" today, "Tue 07:30" this week, "Nov 3 07:30" otherwise.
export function clock(iso) {
  if (!iso) return "";
  const d = new Date(iso), now = new Date();
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (d.toDateString() === now.toDateString()) return hm;
  if (Math.abs(d - now) < 6 * 86400000) return `${DAYS[d.getDay()]} ${hm}`;
  return `${d.toLocaleString("en", { month: "short" })} ${d.getDate()} ${hm}`;
}

export function duration(ms) {
  if (ms == null) return "";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.round((s % 3600) / 60)}m`;
}

// The theme color for a run status.
export function tone(t, status) {
  switch (status) {
    case "succeeded": return t.done;
    case "failed": return t.blocked;
    case "blocked": return t.blocked;
    case "running": return t.working;
    case "invalid": return t.blocked;
    case "paused": return t.dim;
    default: return t.muted;
  }
}

export const word = (status) => (status ?? "").replace(/_/g, " ");

export const when = (run) => run?.started_at ?? run?.scheduled_for ?? run?.created_at;
