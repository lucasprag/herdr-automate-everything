# Requirements

Why this plugin exists: Herdr automations that were ported from Orca on 2026-11-02 ran every entry as an agent, so script automations opened a Claude session in a new space, and no automation ever closed its space. Stale spaces piled up in the spaces panel. The three things that fix that are the gap in every existing plugin (see "Prior art").

This is a fresh plugin, not a fork of DnzzL/herdr-automations. Its design choices (open spaces are the inbox, nothing is cleaned up automatically, the overlap guard is per process) are the ones we disagree with. The forks are a list of problems to expect, not code to merge.

Terminology:
- A *space* is a Herdr desktop app workspace (an `hc` terminal carrying the `herdr.space`, `herdr.tab`, `automation` and `where` labels).
- An *automation* is one entry in the config file. Its *kind* is `script` or `agent`.
- A *run* is one execution of an automation. A *fire* is a scheduled moment it was due.
- The *precheck* is a command that runs before an agent run and decides whether it starts.

## Core behaviours

1. **A script automation runs the script and nothing else.** No agent, no Claude session, no prompt, and no space. It runs headless; its stdout and stderr go into the run's history record. The exit code is the result.
2. **An agent run that succeeds closes what it opened.** If the run created the space, the space closes. If it ran in a space that was already open (see 16), only the tab it opened closes, never the space. The plugin closes only what it can prove it opened: it keeps the IDs it created and checks them before closing.
3. **An agent run that fails or gets blocked stays open.** "Blocked" means the agent is waiting on the user (hc reports `blocked`). Herdr already flags a blocked agent; the plugin adds no notification of its own. The run keeps its space so the user can step in.
4. **A space closed by hand is left alone.** Not reopened, not treated as a failure.
5. **A precheck gate before an agent run.** Orca's convention: exit 0 starts the run, anything else (non-zero, error, timeout) skips it. No agent is started and no space is opened. The skip is recorded with the precheck's exit code, timed-out flag, stdout, stderr and duration, so a broken precheck (a timeout, or exit 126 or 127) can be told apart from "nothing to do" by reading the history.
6. **Worktrees are removed automatically.** When a `worktree`-mode run's space closes, by the plugin or by hand, the plugin removes the worktree with plain `git worktree remove` (no force) and deletes its branch only if that branch has no commits beyond its base or they are already merged or pushed. A dirty worktree or an unpushed branch is kept, and the history record says why.

## When an agent run is finished

7. **Done is proved, not assumed.** A run is *started* only once the agent has accepted the prompt; an agent that never starts or never takes the prompt is a failed run, and the space it opened is closed. A run is *succeeded* when the agent reports `done` with its process still healthy. *Blocked* is not done. *Failed* is the agent process exiting abnormally, never accepting the prompt, or hitting the run timeout.
8. **A run that hits its timeout is stopped.** The agent is killed and the run recorded as failed. Like any failed run its space stays open (behaviour 3), so the user can read what it was doing.

## Scheduling

9. **Cron schedules**, including weekday-only and hour ranges such as `30 8-15 * * 1-5`.
10. **A timezone per automation**, defaulting to the system zone. DST-correct: a fixed-time cron fires once on the night the clocks go back, and is not lost on the night they go forward.
11. **Catch-up with a grace window.** The laptop sleeps. A 7:30 fire missed until 8:10 should still run; a two-minute job missed by an hour should not. Per-automation `grace_minutes`. A fire older than its grace window is recorded as `missed`, and a run of missed fires catches up once, not once per fire.
12. **Fires without the app window open.** The scheduler must survive the app being closed (a launchd job on macOS).
13. **Timeouts**, separately for the precheck and for the run.
14. **No overlapping runs of one automation**, whoever started them. A fire that comes due while the same automation is still running, including a manual run, is recorded as `skipped_overlap`, not queued. A blocked run counts as still running until the user finishes it or closes its space. Different automations may run at once.
15. **One scheduler.** A lock held by the kernel (not a pidfile) keeps a second scheduler from double-firing. The scheduler survives `herdr plugin update` and restarts on the new binary. One invalid entry (a bad cron, a missing script) is reported and skipped; the others keep running.

## Agent runs

16. Per-automation **agent, model, extra agent args, repo, prompt or prompt file**, and two independent choices about where the run happens:
    - **Directory:** the repo itself, or a new git worktree per run from a **base branch** (for example `origin/main`).
    - **Space:** either
      - **a new space per run**, named by the entry: a custom name, defaulting to the automation's name. If a space with that name is still open from an earlier run (a failed or blocked one), the new run gets its own space anyway; names may repeat. Or
      - **a specific space**, named in the entry. The run opens a tab in it, named after the automation. If that space is not open, the run creates it with that name; the plugin then owns it, so a successful run closes it (behaviour 2). Naming the space after the automation gives one shared space that collects that automation's runs as tabs.

    Scripts have no space (behaviour 1), so these choices apply to agent runs only.
17. **Environment.** Scripts, prechecks and agents start with the user's login `PATH`, not launchd's bare one. Per-automation `env`. Every run gets `HERDR_AUTOMATION` (the name) and `HERDR_AUTOMATION_RUN_ID`, so a script can tell it was scheduled.
18. **Manual run** from the panel or the CLI, including on a paused entry. A manual run skips the precheck (as in Orca) and follows the same close and cleanup rules as a scheduled one.
19. **Pause and resume** without deleting the entry, from the panel and the CLI.

## Panel

The panel follows Orca's automations interface: a home page and a history page.

20. **Home page: every automation**, scripts included, one line each with its name, status (`active`, `paused` or `running`), last run (when, and how it ended) and next run. Run history is not on this page. Dragging a line by its handle moves it up or down; the new order is written to the config file, which keeps entries in the order shown.
21. **Actions on each line:** a play button (Run now) and a pause button (Pause; drawn pressed while paused, and clicking it then resumes), plus a `...` menu with Edit and Delete. Delete asks for confirmation, removes the entry from the config file and deletes its run history, as Orca's `remove` does.
22. **Edit opens a form** for the entry's fields. Saving writes back to the config file and changes only that entry: comments, order and every other entry are left as they were. An invalid value is refused in the form, not written.
23. **A History button on the home page** opens the history page.
24. **History page: a log of runs**, newest first, across all automations. One record per fire or run with the automation, trigger (`scheduled` or `manual`), status (`succeeded`, `failed`, `blocked`, `skipped_precheck`, `skipped_overlap`, `missed`) and times. Opening a record shows the precheck result and the run's output; for a blocked run, it jumps to its space. Agent output is captured before the space or tab closes. Skips live in the same log as runs, so "ran and found nothing" is distinguishable from "never fired". No notifications, no read or unread state.
25. **Retention.** History records and captured output are aged out (default: 30 days, a cap per automation).

## Constraints

26. Works with the **desktop app's hc backend**. The TUI backend is not a goal.
27. **One config file** for scripts and agents. Splitting them (gates.json for scripts, automations.yaml for agents) hides the scripts from the panel. The panel and the CLI both write to it, so its format must survive a program rewriting one entry.
28. **No stale spaces or worktrees as a side effect.** The original problem.

## Out of scope

- Remote machines and hosts (Orca has them).
- RRULE schedules and presets; cron only.
- Token and cost tracking per run (Orca shows it).
- Herdr TUI `workflow:` and `session:`.
- Plugin notifications and an inbox of unread runs. History is a log you open when you want it.

## Build steps

Each step ships on its own and replaces part of the current workaround.

1. **Scheduler and script runs.** Config file, cron with timezone and DST, grace and `missed`, timeouts, overlap guard, kernel lock, launchd service, environment, history with output and retention, CLI (`list`, `run`, `pause`, `resume`, `delete`, `history`). Covers 1, 9 to 15, 17, 18 for scripts, 19, 25, 27. Replaces the `run` half of `herdr-automation-gate`.
2. **Agent runs on hc.** Precheck, directory and space choices, base branch, ownership-checked close, done and blocked detection, output capture, worktree removal. Covers 2 to 8, 16, 28. Replaces the rest of the gate and the dnzzl.automations plugin; migrate the five entries and remove `io.smile.herdr-automation-gate` and `dev.dnzzl.herdr-automations`.
3. **The panel.** Home page with the `...` menu, the edit form, the history page. Covers 20 to 24.

## Prior art

- [DnzzL/herdr-automations](https://github.com/DnzzL/herdr-automations) (v0.11.3, 2026-10-30): agent entries, catch-up, overlap guard (per process only), JSONL history, toasts, manual worktree `cleanup`, app panel. Never closes a space. Agents under hc must report their state to herdr-core (`claude`, `pi`; `codex` untested). Fork branches worth reading for the problems they hit:
  - lidong-aesona `feat/command-automations` ([issue #18](https://github.com/DnzzL/herdr-automations/issues/18)): a command kind.
  - rstacruz `rico/close-workspace-on-success` ([issue #13](https://github.com/DnzzL/herdr-automations/issues/13)): close on success.
  - oliverlukschander `golf` and `local/action-reports`: `command:`, `close_when_no_action`, user `PATH` for plugin commands, an agent that never starts closes its space, a bad cron does not panic, DST fall-back fires once.
  - pc035860 `custom`: wait for the agent's TUI and prove it took the prompt, blocked is not done, refuse to close a space it cannot prove it owns, bounded concurrency, run-log ageing, per-automation env, `HERDR_AUTOMATION`, kernel lock.
- Orca automations (`orca automations create --help`): presets, cron or RRULE, timezone, precheck with timeout (gates scheduled runs only, skips recorded as `skipped_precheck` with the full precheck result), missed-run grace, base branch, setup hooks, session reuse, per-run usage and cost.
- [mikedclarke/herdr-shepherd](https://github.com/mikedclarke/herdr-shepherd): `kind="script"`, `gate` with exit 75 = skip, `auto_close`. TUI only, board pane, no catch-up beyond 10 minutes, manual runs not auto-closed.
- mrcndz/herdr-routines: `pre` guard, `shell` type, `close_when_done`. TUI only, stale since 2026-07.
- nakamotosai/herdr-agent-panels task-sched: app panel, interval tasks, any command. Fires only while the window is open, no cron, no gate, no history.
- mkuklis/herdr-launchd: TUI sidebar of launchd jobs. Not a scheduler.
- Mods gallery feed: https://assets.herdr.dev/mods/index.json

## Current workaround (2026-11-02)

A launchd job (`io.smile.herdr-automation-gate`) runs `~/.local/bin/herdr-automation-gate` every minute. `gates.json` holds script entries (`run`) and agent prechecks (`precheck`); passing prechecks call `herdr-automations run <name> --bg`; every `automations.yaml` entry is `disabled: true` so the daemon never fires on its own. The gate also reaps finished runs: saves a transcript to `~/.local/state/herdr/plugins/dnzzl.automations/runs/<run_id>.txt`, then closes the space via `herdr-app do space.close`. This covers scheduling, prechecks and closing spaces, but fails the panel, history and one-config requirements for scripts.

Entries to migrate (all repo `~/Projects/smile/smile-core`, timezone `America/Toronto`):

| Name | Kind | Cron | Grace | Notes |
|---|---|---|---|---|
| auto-approve-linear-prs | script | `*/2 * * * *` | 2 min | 90 s timeout |
| auto-approve-sync-prs | script | `0 * * * *` | 720 min | 60 s timeout |
| controller-migration-loop | agent, `worktree` from `origin/main` | `30 8 * * 1-5` | 120 min | disabled; 240 min run |
| healthy-libyears | agent, `root` | `30 7,13 * * 1-5` | 720 min | 180 min run |
| healthy-libyears-merge | agent, `root` | `30 8-15 * * 1-5` | 50 min | 120 min run, 180 s precheck |
