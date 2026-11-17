---
name: automate-everything
description: Scheduled scripts and agent runs on this computer, macOS or Linux (automate-everything, the Herdr Automations panel), not Orca or cloud routines. Use to schedule something recurring here, to check on automations (is the scheduler up, what ran, why a run failed or was skipped), or to change one (schedule, prompt, pause, resume, run now, delete), here or on other herdr-core machines (`runs_on`, `--on`).
---

# Automations

One binary, `automate-everything`, owns the config, the scheduler (a launchd agent on macOS, a systemd user service on Linux, so it fires with the app closed) and the run history. The Herdr Automations panel is only a view over the same CLI, so anything done here shows up there.

Act through the CLI. `set`, `pause`, `resume` and `delete` validate the whole entry before writing and rewrite only that entry, keeping the file's comments and order. Hand edits to the TOML also work, since the scheduler rereads it every minute, but run `check` after them.

Field reference and defaults: the Config file section of the repo's README, https://github.com/lucasprag/herdr-automate-everything#config-file. Read it before creating an entry.

## Checking on automations

- `status`: whether the scheduler is running, plus the config and state paths. If it isn't running, `automate-everything service ensure` reinstalls and starts it.
- `list --json`: each entry's status (`active`, `paused`, `running` or `invalid`, with the error), its next fire, its last run and any live run.
- `history [name] --json`, then `show <run-id> --json` for one run: the precheck result, exit code, notes, stdout and stderr. For an agent run it also has the tail of the agent's screen.

What a run's status means:

| Status | Meaning |
|---|---|
| `succeeded` | Done. An agent run closed the space it opened. |
| `failed` | Exit code non-zero, timeout, or an agent that quit early. An agent run's space stays open in Herdr so the user can look. |
| `blocked` | The agent stopped to ask something. Its space is open and waiting for the user. |
| `closed` | The user closed the run's space by hand. Not a failure. |
| `skipped_precheck` | The precheck exited non-zero, so the scheduled run never started. |
| `skipped_overlap` | A fire came while the previous run was still going. |
| `missed` | A fire was later than `grace_minutes`, usually because the computer was asleep. |

## Creating and changing

`get <name>` shows the current fields. `set <name> --fields '<json>'` merges the JSON in: keys you leave out are kept, and `null` removes a key, which puts it back to its default. Add `--create` for a new entry. When switching `kind`, null the other kind's fields in the same call.

- **Script or agent:** use `script` for anything a command can do deterministically. Use `agent` only when the work needs judgement. A `precheck` (a cheap command where exit 0 means "there's work") keeps an agent from starting when there's nothing to do. It gates scheduled runs only, never `run`.
- **Agent prompts run unattended.** Write them so the agent finishes without asking anything. A question leaves the run `blocked`, and it keeps the automation's lock until the user answers, so every fire meanwhile is `skipped_overlap`.
- **Worktree runs** (`directory = "worktree"`) get a fresh worktree that is removed once the space closes. A branch with unpushed commits is kept, but the prompt should end in a push or a PR if the work matters.
- **Frequent agent schedules** (hourly or more often) should use `space = "shared"`, so runs open as tabs in one space instead of filling the sidebar.
- **A new entry goes live at its next matching minute.** Past fires never run, so there's no backfill. Prove the entry first with `run <name> --wait`: it runs even while paused and skips the precheck. For an agent, `--wait` lasts as long as the agent does, so use a plain `run` and follow it with `history`.
- **A machine without automate-everything:** `machine = "<name>"` (one of `automate-everything machines`) runs the entry on that herdr-core machine while this Mac keeps scheduling it, so nothing runs while this Mac sleeps; prefer `runs_on` when the machine runs automate-everything. `command`, `cwd`, `repo`, `precheck` and `prompt_file` are paths on that machine (`~` is its home), so check them there (`hc exec --on <name> ls <path>`), not here. An unreachable machine fails the run with a note; `check` reports unknown machines and missing paths.
- **Running on other machines** (each fires its own copy, whether or not this Mac is awake): `runs_on = ["lab"]` (or `["local", "lab"]` for both) sends that machine a copy, and its own scheduler fires it. `machines --probe` shows which machines run automate-everything. Define and change it here, never on the machine (its copy refuses changes). Paths are the machine's (`~` is its home), and a path can differ between machines, so an entry running in more than one place needs it to exist in each. After a change, `check` asks each machine in its runs_on whether it can run its copy and reports what it can't (``on `lab`: ...``). A change reaches the machine right away, or within a minute of it coming back online; `set` warns when it couldn't. Its runs and history stay there: `automate-everything --on lab history <name>`, `--on lab show <run-id>`. `run <name>` runs it everywhere it runs; `run --wait` needs one place (`--on lab run <name> --wait`).
- **Fanning out from an agent run:** `automate-everything tab open` (see `--help`) starts a helper as another tab in the run's space (`--space <id>` for another space, `--new-space <name>` for a new one), and `tab close --self` is the helper's last command. When the run succeeds, the runner closes only the run's own tab; the space closes once its last tab does.

## Pausing, resuming, deleting

`pause` keeps the entry and its history. `delete --yes` removes both, so confirm with the user first (`--keep-history` removes only the entry). Add `--on <machine>` for an entry another machine schedules.

Entries ported from Orca start paused. Before resuming one, check that Orca no longer fires it (`orca automations list`); otherwise it runs twice.
