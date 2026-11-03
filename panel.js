// The side panel: a home page (every automation, one line each) and a
// history page (every fire and run, newest first), with a run's details.

import { div, svg } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { theme, run, spaces, openOverlay, home, appHome, watchFolder, nextCommand, exec, readTextFile } from "herdr";
import * as cli from "./cli.js";
import { ago, clock, duration, tone, word, when } from "./format.js";

const REFRESH_MS = 15000;
const INSTALL_MS = 5 * 60 * 1000;
const MOD_DIR = `${appHome()}/mods/automate-everything`;
const OUTPUT_LINES = 400;
const ICON = "mods/automate-everything";
const CHROME = "mods/workspace-chrome";

const STATE_DIR = `${home()}/.local/state/automate-everything`;
const CONFIG_DIR = `${home()}/.config/automate-everything`;

export class Panel {
  init(cx) {
    this.page = "home"; // home | history | run
    this.items = null;
    this.runs = null;
    this.detail = null;
    this.filter = null; // automation name the history is narrowed to
    this.error = null;
    this.scheduler = null;
    this.busy = false;
    this.install = null; // { ok, log } from install.sh's last run
    // Self-install: build if src/ changed, put the binary in ~/.local/bin,
    // keep the launchd agent running. Cheap when nothing changed.
    this.selfInstall(cx);
    cx.timer.every(INSTALL_MS, (cx) => this.selfInstall(cx));
    this.refresh(cx);
    cx.timer.every(REFRESH_MS, (cx) => this.refresh(cx));
    // keys.json: "automate-everything.history", ".home", ".new", ".edit:<name>", ".menu:<name>".
    cx.spawn(async (cx) => {
      for (;;) {
        const c = await nextCommand();
        const [name, arg] = c.split(":");
        if (name === "history" || name === "home") this.go(name, cx, { filter: arg ?? null });
        else if (name === "new") openOverlay(editSpec(null));
        else if (name === "edit" && arg) openOverlay(editSpec(arg));
        else if (name === "run" && arg) this.openRun(arg, cx);
        else if (name === "menu" && arg) {
          const item = this.items?.find((i) => i.name === arg);
          if (item) openOverlay(menuSpec(item));
        }
      }
    });
    for (const dir of [STATE_DIR, CONFIG_DIR]) {
      cx.spawn(async (cx) => {
        for (;;) {
          try {
            await watchFolder(dir);
          } catch {
            return; // not there yet; the timer still refreshes
          }
          this.refresh(cx);
        }
      });
    }
  }

  selfInstall(cx) {
    cx.spawn(async () => {
      try {
        // Detached: a first build can take longer than exec allows.
        await exec("/bin/sh", ["-c", `mkdir -p '${MOD_DIR}/.target' && nohup '${MOD_DIR}/install.sh' > '${MOD_DIR}/.target/install.log' 2>&1 &`]);
      } catch {}
    });
  }

  async readInstall() {
    const status = (await readTextFile(`${MOD_DIR}/.target/install.status`))?.trim();
    const log = (await readTextFile(`${MOD_DIR}/.target/install.log`)) ?? "";
    this.install = status == null ? null : { ok: status === "0", log: log.trim().split("\n").slice(-3).join(" ") };
  }

  refresh(cx) {
    if (this.busy) {
      this.again = true;
      return;
    }
    this.busy = true;
    cx.spawn(async (cx) => {
      try {
        do {
          this.again = false;
          await this.readInstall().catch(() => {});
          const [items, st] = await Promise.all([cli.list(), cli.status()]);
          this.items = items;
          this.scheduler = st;
          if (this.page !== "home") this.runs = await cli.history();
          if (this.page === "run" && this.detail) this.detail = await cli.show(this.detail.id);
          this.error = null;
        } while (this.again);
      } catch (e) {
        this.error = String(e?.message ?? e);
      }
      this.busy = false;
      cx.notify();
    });
  }

  go(page, cx, extra = {}) {
    Object.assign(this, { page, ...extra });
    cx.notify();
    this.refresh(cx);
  }

  openRun(id, cx) {
    cx.spawn(async (cx) => {
      try {
        this.detail = await cli.show(id);
        this.page = "run";
      } catch (e) {
        this.error = String(e?.message ?? e);
      }
      cx.notify();
    });
  }

  iconButton(t, id, icon, onClick, opts = {}) {
    const el = div().id(id).size(26).flex_none().flex().items_center().justify_center().text_color(t.dim)
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_click(onClick);
    const glyph = svg(icon).size(opts.size ?? 14);
    return el.child(opts.spin ? glyph.spin(900) : glyph);
  }

  header(t, cx) {
    const back = (to) => this.iconButton(t, "ae-back", `${CHROME}/chevron-left.svg`, (_e, cx) => this.go(to, cx));
    const bar = h_flex().h(42).flex_none().px(10).gap(6).items_center().border_b(1).border_color(t.border);
    const refresh = this.iconButton(t, "ae-refresh", `${ICON}/refresh.svg`, (_e, cx) => this.refresh(cx), { spin: this.busy, size: 13 });
    if (this.page === "home") {
      const items = this.items ?? [];
      const running = items.filter((i) => i.status === "running").length;
      return bar.pl(14)
        .child(div().font_semibold().text_color(t.foreground).child("Automations"))
        .when(this.items !== null, (el) => el.child(div().text_size(11.5).text_color(t.dim)
          .child(`${items.length}${running ? ` · ${running} running` : ""}`)))
        .child(div().flex_1())
        .child(this.iconButton(t, "ae-new", `${CHROME}/plus.svg`, () => openOverlay(editSpec(null))))
        .child(h_flex().id("ae-history").h(26).px(8).gap(5).flex_none().items_center().text_size(12).text_color(t.muted)
          .hover((s) => s.bg(t.hover).text_color(t.foreground))
          .on_click((_e, cx) => this.go("history", cx, { filter: null }))
          .child(svg(`${CHROME}/clock.svg`).size(13)).child("History"))
        .child(refresh);
    }
    if (this.page === "history") {
      return bar
        .child(back("home"))
        .child(div().font_semibold().text_color(t.foreground).child("History"))
        .when(Boolean(this.filter), (el) => el.child(h_flex().id("ae-filter").h(22).px(7).gap(5).items_center()
          .border_1().border_color(t.border).text_size(11.5).text_color(t.muted)
          .hover((s) => s.text_color(t.foreground))
          .on_click((_e, cx) => { this.filter = null; cx.notify(); })
          .child(this.filter).child(svg(`${CHROME}/x.svg`).size(10))))
        .child(div().flex_1())
        .child(refresh);
    }
    const r = this.detail;
    return bar
      .child(back("history"))
      .child(div().flex_1().min_w(0).truncate().font_semibold().text_color(t.foreground).child(r?.automation ?? "Run"))
      .child(refresh);
  }

  banner(t) {
    const notRunning = this.scheduler && !this.scheduler.scheduler_running;
    const text = this.error
      ?? (this.install && !this.install.ok ? `Couldn't install the scheduler: ${this.install.log} (see mods/automate-everything/.target/install.log)` : null)
      ?? (notRunning ? "Starting the scheduler…" : null);
    if (!text) return null;
    return div().px(14).py(7).flex_none().text_size(11.5).text_color(t.blocked).border_b(1).border_color(t.border).child(text);
  }

  // ---- home -------------------------------------------------------------

  statusOf(item) {
    if (item.live?.status === "blocked") return "blocked";
    return item.status; // active | paused | running | invalid
  }

  mark(t, status) {
    if (status === "running") return svg(`${CHROME}/spinner.svg`).size(12).flex_none().text_color(t.working).spin(900);
    const color = status === "active" ? t.done : tone(t, status);
    return div().size(12).flex_none().flex().items_center().justify_center()
      .child(div().size(7).rounded(4).when(status === "paused", (el) => el.border_1().border_color(color)).when(status !== "paused", (el) => el.bg(color)));
  }

  itemRow(t, item) {
    const status = this.statusOf(item);
    const last = item.last;
    const lastText = last ? `${word(last.status)} ${ago(when(last))}` : "never ran";
    const next = item.next && status !== "paused" ? `next ${clock(item.next)}` : "";
    return v_flex().id(`ae-item-${item.name}`).px(16).py(8).gap(3).flex_none().cursor_pointer()
      .border_b(1).border_color(t.border)
      .hover((s) => s.bg(t.hover))
      .on_click((_e, cx) => this.go("history", cx, { filter: item.name }))
      .child(h_flex().gap(8).items_center()
        .child(this.mark(t, status))
        .child(div().flex_1().min_w(0).truncate().text_size(12.8).text_color(t.foreground).child(item.name))
        .child(div().flex_none().text_size(11.5).text_color(t.dim).child(next))
        .child(div().id(`ae-more-${item.name}`).size(22).flex_none().flex().items_center().justify_center().text_color(t.dim)
          .hover((s) => s.bg(t.selected).text_color(t.foreground))
          .on_click((_e, cx) => {
            cx.stop_propagation();
            openOverlay(menuSpec(item));
          })
          .child(svg(`${ICON}/dots.svg`).size(14))))
      .child(h_flex().pl(20).gap(6).items_center().text_size(11.5)
        .child(div().flex_none().text_color(tone(t, status)).child(status))
        .child(div().flex_none().text_color(t.dim).child(`· ${item.kind ?? "?"} ·`))
        .child(div().flex_1().min_w(0).truncate().text_color(last ? tone(t, last.status) : t.dim).child(lastText)))
      .when(Boolean(item.error), (el) => el.child(div().pl(20).text_size(11.5).text_color(t.blocked).child(item.error)));
  }

  home(t) {
    if (this.items === null) return div().px(14).py(10).text_size(12).text_color(t.dim).child(this.error ? "" : "Loading…");
    if (!this.items.length) {
      return v_flex().px(16).py(14).gap(6).text_size(12).text_color(t.dim)
        .child("No automations yet.")
        .child("Add one with + or in ~/.config/automate-everything/automations.toml.");
    }
    return v_flex().id("ae-list").flex_1().min_h(0).overflow_y_scrollbar().children(this.items.map((i) => this.itemRow(t, i)));
  }

  // ---- history ----------------------------------------------------------

  runRow(t, r) {
    const bits = [r.trigger, r.kind];
    if (r.duration_ms != null) bits.push(duration(r.duration_ms));
    if (r.missed_fires > 1) bits.push(`${r.missed_fires} fires`);
    if (r.caught_up) bits.push(`caught up ${r.caught_up}`);
    return v_flex().id(`ae-run-${r.id}`).px(16).py(7).gap(2).flex_none().cursor_pointer()
      .border_b(1).border_color(t.border)
      .hover((s) => s.bg(t.hover))
      .on_click((_e, cx) => this.openRun(r.id, cx))
      .child(h_flex().gap(8).items_center()
        .child(this.mark(t, r.status === "running" ? "running" : r.status === "blocked" ? "blocked" : r.status))
        .child(div().flex_1().min_w(0).truncate().text_size(12.8).text_color(t.foreground).child(r.automation))
        .child(div().flex_none().text_size(11.5).text_color(t.dim).child(clock(when(r)))))
      .child(h_flex().pl(20).gap(6).text_size(11.5)
        .child(div().flex_none().text_color(tone(t, r.status)).child(word(r.status)))
        .child(div().flex_1().min_w(0).truncate().text_color(t.dim).child(`· ${bits.filter(Boolean).join(" · ")}`)));
  }

  history(t) {
    if (this.runs === null) return div().px(14).py(10).text_size(12).text_color(t.dim).child("Loading…");
    const runs = this.filter ? this.runs.filter((r) => r.automation === this.filter) : this.runs;
    if (!runs.length) return div().px(16).py(14).text_size(12).text_color(t.dim).child("Nothing has fired yet.");
    return v_flex().id("ae-runs").flex_1().min_h(0).overflow_y_scrollbar().children(runs.map((r) => this.runRow(t, r)));
  }

  // ---- one run ------------------------------------------------------------

  field(t, label, value, color) {
    if (value == null || value === "") return null;
    return h_flex().gap(10).items_start().text_size(12)
      .child(div().w(84).flex_none().text_color(t.dim).child(label))
      .child(div().flex_1().min_w(0).text_color(color ?? t.foreground).child(String(value)));
  }

  block(t, title, text) {
    if (!text || !String(text).trim()) return null;
    let lines = String(text).replace(/\s+$/, "").split("\n");
    const cut = lines.length > OUTPUT_LINES ? lines.length - OUTPUT_LINES : 0;
    if (cut) lines = lines.slice(cut);
    return v_flex().gap(4)
      .child(div().text_size(11.5).font_medium().text_color(t.muted).child(cut ? `${title} (last ${OUTPUT_LINES} lines)` : title))
      .child(v_flex().p(8).bg(t.background).border_1().border_color(t.border)
        .font_family("JetBrains Mono").text_size(11).text_color(t.foreground)
        .children(lines.map((l, i) => div().id(`ln-${title}-${i}`).min_h(14).child(l || " "))));
  }

  detailView(t) {
    const r = this.detail;
    if (!r) return null;
    const a = r.agent;
    const p = r.precheck;
    const spaceOpen = a?.space_id && spaces().some((s) => s.id === a.space_id);
    const exit = r.exit_code != null ? String(r.exit_code) : r.signal != null ? `signal ${r.signal}` : null;
    return v_flex().id("ae-detail").flex_1().min_h(0).overflow_y_scrollbar().px(16).py(12).gap(12)
      .child(v_flex().gap(5).children([
        this.field(t, "Status", word(r.status), tone(t, r.status)),
        this.field(t, "Trigger", r.trigger),
        this.field(t, "Scheduled", r.scheduled_for && new Date(r.scheduled_for).toLocaleString()),
        this.field(t, "Missed", r.missed_fires && `${r.missed_fires} fire(s), from ${new Date(r.first_missed).toLocaleString()}`),
        this.field(t, "Caught up", r.caught_up && `${r.caught_up} earlier fire(s)`),
        this.field(t, "Started", r.started_at && new Date(r.started_at).toLocaleString()),
        this.field(t, "Finished", r.finished_at && new Date(r.finished_at).toLocaleString()),
        this.field(t, "Duration", duration(r.duration_ms)),
        this.field(t, "Exit", exit),
        this.field(t, "Timed out", r.timed_out ? "yes" : null, t.blocked),
        this.field(t, "Note", r.note),
        this.field(t, "Agent", a && `${a.agent} in ${a.cwd.replace(home(), "~")}`),
        this.field(t, "Space", a?.space_name && `${a.space_name}${a.space_created ? "" : " (shared)"}`),
        this.field(t, "Closed", a?.closed),
        this.field(t, "Worktree", a?.worktree_cleanup ?? a?.worktree?.replace(home(), "~")),
      ].filter(Boolean)))
      .when(Boolean(spaceOpen), (el) => el.child(h_flex().child(h_flex().id("ae-goto").h(28).px(12).items_center()
        .bg(t.solid).text_color(t.solid_foreground).text_size(12).cursor_pointer().hover((s) => s.opacity(0.88))
        .on_click(() => run("space.select", { id: a.space_id }))
        .child(r.status === "blocked" ? "Go to its space (it needs you)" : "Go to its space"))))
      .when(Boolean(p), (el) => el.child(v_flex().gap(6).children([
        div().text_size(12).font_semibold().text_color(t.foreground).child("Precheck"),
        this.field(t, "Command", p.command),
        this.field(t, "Exit", p.exit_code != null ? String(p.exit_code) : p.signal != null ? `signal ${p.signal}` : "none",
          p.exit_code === 0 ? t.done : t.blocked),
        this.field(t, "Timed out", p.timed_out ? "yes" : null, t.blocked),
        this.field(t, "Duration", duration(p.duration_ms)),
        this.block(t, "precheck stdout", p.stdout),
        this.block(t, "precheck stderr", p.stderr),
      ].filter(Boolean))))
      .children([
        this.block(t, "stdout", r.output?.stdout),
        this.block(t, "stderr", r.output?.stderr),
        this.block(t, "agent output", r.output?.agent),
      ].filter(Boolean));
  }

  render(cx) {
    const t = theme();
    const body = this.page === "home" ? this.home(t) : this.page === "history" ? this.history(t) : this.detailView(t);
    return v_flex().id("ae-root").size_full().min_h(0).bg(t.chrome).overflow_hidden().text_size(13)
      .children([this.header(t, cx), this.banner(t), body].filter(Boolean));
  }
}

function menuSpec(item) {
  return { frame: "none", mod: "automate-everything", kind: "popup", width: 180,
    props: { view: "menu", name: item.name, paused: item.paused, invalid: item.status === "invalid" } };
}

export function editSpec(name) {
  return { frame: "none", mod: "automate-everything", kind: "dialog", width: 640, height: 0.86,
    props: { view: "edit", name } };
}
