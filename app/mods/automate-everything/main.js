// automate-everything: the panel for scheduled scripts and agent runs.
// The scheduler, runs, config and history live in the `automate-everything`
// binary (src/, installed by install.sh); this mod only shows them and acts
// through its CLI.
//
// One mod, four views, picked by props().view: the side panel (no view), the
// `...` menu, the edit form and the delete confirmation (popups of this mod).

import { View } from "gpui-kit";
import { props } from "herdr";
import { Panel } from "./panel.js";
import { Menu, Confirm } from "./popups.js";
import { Edit } from "./edit.js";

const VIEWS = { menu: Menu, edit: Edit, confirm: Confirm };

export default class AutomateEverything extends View {
  init(_props, cx) {
    const Kind = VIEWS[props()?.view] ?? Panel;
    this.view = new Kind();
    this.view.init(cx);
  }

  render(cx) {
    return this.view.render(cx);
  }
}
