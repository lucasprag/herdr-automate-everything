// The panel's popups: the `...` menu on each automation, and the delete
// confirmation. Both act through the CLI and close; the panel follows the
// files changing.

import { div, svg } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { theme, props, closeOverlay, openOverlay } from "herdr";
import * as cli from "./cli.js";
import { editSpec } from "./panel.js";

const frame = (t) => v_flex().bg(t.popover).border_1().border_color(t.border).shadow_lg().text_size(13);

export class Menu {
  init(cx) {
    this.error = null;
    this.handle = cx.focus_handle();
    this.handle.focus();
  }

  act(fn, cx) {
    cx.spawn(async (cx) => {
      try {
        await fn();
        closeOverlay(props().overlay);
      } catch (e) {
        this.error = String(e?.message ?? e);
        cx.notify();
      }
    });
  }

  render() {
    const t = theme();
    const p = props() ?? {};
    const item = (id, label, onClick, color) => h_flex().id(id).h(30).px(14).items_center().cursor_pointer()
      .text_color(color ?? t.foreground).hover((s) => s.bg(t.hover))
      .on_click(onClick).child(label);
    const items = [];
    if (!p.invalid) items.push(item("m-run", "Run now", (_e, cx) => this.act(() => cli.runNow(p.name), cx)));
    items.push(item("m-edit", "Edit", () => { closeOverlay(p.overlay); openOverlay(editSpec(p.name)); }));
    if (!p.invalid) {
      items.push(p.paused
        ? item("m-resume", "Resume", (_e, cx) => this.act(() => cli.resume(p.name), cx))
        : item("m-pause", "Pause", (_e, cx) => this.act(() => cli.pause(p.name), cx)));
    }
    items.push(item("m-delete", "Delete…", () => {
      closeOverlay(p.overlay);
      openOverlay({ frame: "none", mod: "automate-everything", kind: "dialog", width: 420, height: 170, props: { view: "confirm", name: p.name } });
    }, t.blocked));
    return frame(t).id("ae-menu").track_focus(this.handle).w_full().py(4)
      .children(items)
      .when(Boolean(this.error), (el) => el.child(div().px(14).py(6).text_size(11.5).text_color(t.blocked).child(this.error)));
  }
}

export class Confirm {
  init(cx) {
    this.error = null;
    this.busy = false;
    this.handle = cx.focus_handle();
    this.handle.focus();
  }

  yes(cx) {
    if (this.busy) return;
    this.busy = true;
    cx.spawn(async (cx) => {
      try {
        await cli.remove(props().name);
        closeOverlay(props().overlay);
      } catch (e) {
        this.error = String(e?.message ?? e);
        this.busy = false;
        cx.notify();
      }
    });
  }

  render() {
    const t = theme();
    const p = props() ?? {};
    const button = (id, label, bg, fg, border, onClick) => h_flex().id(id).h(32).px(14).items_center()
      .border_1().border_color(border).bg(bg).text_color(fg).cursor_pointer().hover((s) => s.opacity(0.88))
      .on_click(onClick).child(label);
    return frame(t).id("ae-confirm").size_full().min_h(0).p(18).gap(12)
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "enter") this.yes(cx);
        else if (e.key === "escape") closeOverlay(props().overlay);
        else return;
        cx.stop_propagation();
      })
      .child(h_flex().items_center()
        .child(div().flex_1().text_size(15).font_semibold().text_color(t.foreground).child(`Delete ${p.name}?`))
        .child(div().text_size(11).text_color(t.dim).child("Esc")))
      .child(div().flex_1().min_h(0).text_color(t.muted)
        .child(this.error ?? "Removes it from automations.toml and deletes its run history. Its open spaces stay."))
      .child(h_flex().justify_end().gap(8).text_size(12)
        .child(button("no", "Cancel", t.raised, t.foreground, t.input, () => closeOverlay(props().overlay)))
        .child(button("yes", this.busy ? "Deleting…" : "Delete", t.blocked, t.solid_foreground, t.blocked, (_e, cx) => this.yes(cx))));
  }
}
