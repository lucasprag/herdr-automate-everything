// The edit form: one entry's fields. Save sends them to `automate-everything
// set`, which validates and rewrites only that entry; anything invalid comes
// back as an error here and nothing is written.

import { div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextareaState, Textarea } from "gpui-base";
import { theme, props, closeOverlay, settings } from "herdr";
import * as cli from "./cli.js";

// [key, label, placeholder, kind ("all" | "script" | "agent"), type]
const FIELDS = [
  ["description", "Description", "What it does (optional)", "all", "text"],
  ["schedule", "Schedule", "Cron: minute hour day month weekday, e.g. 30 8-15 * * 1-5", "all", "text"],
  ["timezone", "Timezone", "e.g. America/Toronto (empty: this computer's)", "all", "text"],
  ["grace_minutes", "Grace (minutes)", "How late a missed fire may still run (default 10)", "all", "int"],
  ["command", "Command", "Shell command, e.g. ~/.local/bin/my-script --flag", "script", "text"],
  ["cwd", "Folder", "Where it runs (empty: home)", "script", "text"],
  ["repo", "Repo", "e.g. ~/Projects/my-app", "agent", "text"],
  ["base_branch", "Base branch", "For worktrees (default origin/main)", "worktree", "text"],
  ["space_name", "Space name", "Empty: the automation's name", "agent", "text"],
  ["agent", "Agent", "Program to run (default claude)", "agent", "text"],
  ["model", "Model", "Passed as --model (optional)", "agent", "text"],
  ["agent_args", "Agent args", "Extra arguments, space-separated (optional)", "agent", "args"],
  ["prompt", "Prompt", "What the agent should do", "agent", "long"],
  ["prompt_file", "Prompt file", "Or a file to read the prompt from", "agent", "text"],
  ["precheck", "Precheck", "Command; exit 0 starts a scheduled run, anything else skips it", "agent", "text"],
  ["precheck_timeout_seconds", "Precheck timeout (s)", "Default 120", "agent", "int"],
  ["timeout_seconds", "Run timeout (s)", "Default 3600 for scripts, 10800 for agents", "all", "int"],
  ["env", "Environment", "KEY=value, one per line (optional)", "all", "env"],
];

const CHOICES = {
  kind: [["script", "Script"], ["agent", "Agent"]],
  directory: [["repo", "The repo"], ["worktree", "New worktree per run"]],
  space: [["per-run", "New space per run"], ["shared", "One shared space"]],
};

function toText(key, v) {
  if (v == null) return "";
  if (key === "env") return Object.entries(v).map(([k, x]) => `${k}=${x}`).join("\n");
  if (Array.isArray(v)) return v.join(" ");
  return String(v);
}

export class Edit {
  init(cx) {
    const p = props() ?? {};
    this.creating = !p.name;
    this.error = null;
    this.saving = false;
    this.loaded = this.creating;
    this.choice = { kind: "script", directory: "repo", space: "per-run" };
    this.handle = cx.focus_handle();
    this.nameInput = InputState.new({ placeholder: "my-automation (letters, digits, - _ .)" });
    this.inputs = {};
    for (const [key, , placeholder, , type] of FIELDS) {
      if (type === "long" || type === "env") {
        const s = TextareaState.new({ placeholder, rows: type === "long" ? 5 : 3 });
        s.set_auto_grow(type === "long" ? 5 : 2, type === "long" ? 14 : 6);
        this.inputs[key] = s;
      } else {
        this.inputs[key] = InputState.new({ placeholder });
      }
    }
    if (this.creating) {
      this.nameInput.focus();
      return;
    }
    cx.spawn(async (cx) => {
      try {
        const entry = await cli.get(p.name);
        this.choice.kind = entry.kind ?? "script";
        this.choice.directory = entry.directory ?? "repo";
        this.choice.space = entry.space ?? "per-run";
        for (const [key] of FIELDS) this.inputs[key].set_value(toText(key, entry[key]));
        // What the form shows now, so Save sends only what changed.
        try { this.original = this.fields(); } catch { this.original = {}; }
        this.loaded = true;
      } catch (e) {
        this.error = String(e?.message ?? e);
      }
      cx.notify();
    });
  }

  close() { closeOverlay(props().overlay); }

  shown(scope) {
    if (scope === "all") return true;
    if (scope === "worktree") return this.choice.kind === "agent" && this.choice.directory === "worktree";
    return this.choice.kind === scope;
  }

  // The fields to send: every field, null where empty or not for this kind,
  // so `set` removes what was cleared.
  fields() {
    const out = { kind: this.choice.kind };
    const agent = this.choice.kind === "agent";
    out.directory = agent && this.choice.directory === "worktree" ? "worktree" : null;
    out.space = agent && this.choice.space === "shared" ? "shared" : null;
    for (const [key, label, , scope, type] of FIELDS) {
      const raw = this.inputs[key].value();
      if (!this.shown(scope) || !raw.trim()) {
        out[key] = null;
        continue;
      }
      if (type === "int") {
        if (!/^\d+$/.test(raw.trim())) throw new Error(`${label} must be a whole number`);
        out[key] = parseInt(raw.trim(), 10);
      } else if (type === "args") {
        out[key] = raw.trim().split(/\s+/);
      } else if (type === "env") {
        const env = {};
        for (const line of raw.split("\n").map((l) => l.trim()).filter(Boolean)) {
          const i = line.indexOf("=");
          if (i <= 0) throw new Error(`Environment: "${line}" is not KEY=value`);
          env[line.slice(0, i).trim()] = line.slice(i + 1);
        }
        out[key] = env;
      } else if (type === "long") {
        out[key] = raw;
      } else {
        out[key] = raw.trim();
      }
    }
    return out;
  }

  save(cx) {
    if (this.saving || !this.loaded) return;
    let name, fields;
    try {
      name = this.creating ? this.nameInput.value().trim() : props().name;
      if (!name) throw new Error("Give it a name");
      fields = this.fields();
      if (this.original) {
        for (const k of Object.keys(fields)) {
          if (JSON.stringify(fields[k]) === JSON.stringify(this.original[k])) delete fields[k];
        }
        if (!Object.keys(fields).length) return this.close();
      }
    } catch (e) {
      this.error = String(e?.message ?? e);
      cx.notify();
      return;
    }
    this.saving = true;
    this.error = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        await cli.save(name, fields, this.creating);
        this.close();
      } catch (e) {
        this.error = String(e?.message ?? e);
        this.saving = false;
        cx.notify();
      }
    });
  }

  segmented(t, key) {
    return h_flex().gap(0).children(CHOICES[key].map(([value, label]) => {
      const on = this.choice[key] === value;
      return h_flex().id(`seg-${key}-${value}`).h(30).px(12).items_center().cursor_pointer()
        .border_1().border_color(on ? t.accent : t.input).bg(on ? t.selected : t.background)
        .text_size(12).text_color(on ? t.foreground : t.muted)
        .hover((s) => s.text_color(t.foreground))
        .on_click((_e, cx) => { this.choice[key] = value; cx.notify(); })
        .child(label);
    }));
  }

  row(t, label, control) {
    return v_flex().gap(5).child(div().text_size(11.5).text_color(t.dim).child(label)).child(control);
  }

  input(t, key) {
    const state = this.inputs[key];
    const type = FIELDS.find((f) => f[0] === key)[4];
    if (type === "long" || type === "env") {
      return div().id(`f-${key}`).px(10).py(6).border_1().border_color(t.input).bg(t.background)
        .on_click(() => state.focus())
        .child(Textarea.new(state).w_full().text_size(12.5).text_color(t.foreground)
          .when(type === "env", (el) => el.font_family(settings().font_family ?? "JetBrains Mono")));
    }
    return h_flex().id(`f-${key}`).h(34).px(10).items_center().border_1().border_color(t.input).bg(t.background)
      .on_click(() => state.focus())
      .child(Input.new(state).flex_1().h_full().text_size(12.5).text_color(t.foreground));
  }

  render() {
    const t = theme();
    const p = props() ?? {};
    const rows = [];
    if (this.creating) rows.push(this.row(t, "Name", h_flex().id("f-name").h(34).px(10).items_center().border_1().border_color(t.input).bg(t.background)
      .on_click(() => this.nameInput.focus()).child(Input.new(this.nameInput).flex_1().h_full().text_size(12.5).text_color(t.foreground))));
    rows.push(this.row(t, "Kind", this.segmented(t, "kind")));
    for (const [key, label, , scope] of FIELDS) {
      if (key === "repo" && this.shown("agent")) {
        rows.push(this.row(t, label, this.input(t, key)));
        rows.push(this.row(t, "Runs in", this.segmented(t, "directory")));
        continue;
      }
      if (key === "space_name" && this.shown("agent")) {
        rows.push(this.row(t, "Space", this.segmented(t, "space")));
      }
      if (this.shown(scope)) rows.push(this.row(t, label, this.input(t, key)));
    }
    const button = (id, label, bg, fg, border, onClick) => h_flex().id(id).h(32).px(14).items_center()
      .border_1().border_color(border).bg(bg).text_color(fg).cursor_pointer().hover((s) => s.opacity(0.88))
      .on_click(onClick).child(label);
    return v_flex().id("ae-edit").track_focus(this.handle).size_full().min_h(0)
      .bg(t.popover).border_1().border_color(t.border).shadow_lg().text_size(13)
      .on_key_down((e, cx) => {
        if (e.key === "escape") this.close();
        else if (e.key === "enter" && (e.modifiers?.platform || e.modifiers?.control)) this.save(cx);
        else return;
        cx.stop_propagation();
      })
      .child(h_flex().px(18).pt(16).pb(10).items_center()
        .child(div().flex_1().text_size(15).font_semibold().text_color(t.foreground).child(this.creating ? "New automation" : `Edit ${p.name}`))
        .child(div().text_size(11).text_color(t.dim).child("Cmd+Enter: save · Esc")))
      .child(v_flex().id("ae-edit-fields").flex_1().min_h(0).overflow_y_scrollbar().px(18).pb(12).gap(12)
        .children(this.loaded ? rows : [div().text_color(t.dim).child(this.error ? "" : "Loading…")]))
      .when(Boolean(this.error), (el) => el.child(div().px(18).py(8).text_size(12).text_color(t.blocked).border_t(1).border_color(t.border).child(this.error)))
      .child(h_flex().px(18).py(12).gap(8).justify_end().border_t(1).border_color(t.border).text_size(12)
        .child(button("cancel", "Cancel", t.raised, t.foreground, t.input, () => this.close()))
        .child(button("save", this.saving ? "Saving…" : "Save", t.solid, t.solid_foreground, t.solid, (_e, cx) => this.save(cx))));
  }
}
