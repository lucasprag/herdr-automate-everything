# Automate everything

Run scripts and AI agents on a schedule from Herdr. They keep running on time
even when the app is closed.

A few things you could set up:

- Every weekday at 9:00, run a script that pulls the latest code in all your repos.
- Every morning, have Claude review yesterday's merged pull requests in a fresh
  worktree and leave a summary.
- Every hour during work hours, have an agent check for failing builds, but only
  start it when there's actually something to look at.

## Getting started

1. Clone this repo into your Herdr mods folder:

   ```sh
   git clone https://github.com/lucasprag/herdr-automate-everything ~/.config/herdr-app/mods/automate-everything
   ```

2. Open **Automations** in Herdr's side panel. The first time, it downloads
   its program from the repo's releases and installs it in the background,
   which takes a few seconds. Nothing else to install.
3. Click **+**, give the automation a name, pick **Script** or **Agent**, set a
   schedule and save.
4. Click the play button next to it to try it right away instead of waiting
   for the schedule.

From then on it runs by itself. Updates install themselves too: pull the repo
and the panel takes care of the rest.

## Scripts and agents

A **script** automation runs a shell command, like `~/bin/backup-notes`. You
don't see it run. Its output and whether it worked go into the history.

An **agent** automation starts Claude (or another coding agent) with a prompt
you write, in a repo you pick. It opens in a new space in Herdr, so you can
watch it work or check on it later. When the agent finishes, its space closes
by itself. If something goes wrong, the space stays open so you can see what
happened.

Use a script when a command can do the job. Use an agent when the work needs
judgement.

A few options make agents nicer to live with:

- **New worktree per run** gives the agent its own copy of the repo, so it never
  touches what you're working on. The worktree is deleted afterwards.
- **One shared space** puts every run as a tab in a single space, which is handy
  for agents that run often so they don't fill up your sidebar.
- **Precheck** is a quick command that decides whether the agent should start at
  all. If it exits with 0, the agent runs; otherwise the run is skipped. That
  way you don't pay for an agent that has nothing to do.

Write agent prompts so the agent can finish without asking you anything.
Nobody is there to answer when it runs at 7 in the morning.

## Schedules

Schedules use cron's five fields: minute, hour, day of the month, month and
day of the week.

| Schedule | Runs |
|---|---|
| `0 9 * * *` | every day at 9:00 |
| `30 8 * * 1-5` | weekdays at 8:30 |
| `0 9-17 * * 1-5` | every hour from 9:00 to 17:00 on weekdays |
| `*/15 * * * *` | every 15 minutes |
| `0 7 1 * *` | at 7:00 on the first of every month |
| `@daily` | every day at midnight (also `@hourly`, `@weekly`) |

Times are in your computer's timezone unless you set another one.

If your Mac was asleep when an automation was due, it runs once when the Mac
wakes up, as long as that's within 10 minutes of the scheduled time (you can
change that with **Grace**). Otherwise the run is recorded as missed and waits
for its next time.

## The panel

Each automation has three buttons:

- **Play** runs it now.
- **Pause** stops it from running on its schedule until you click it again.
- **...** lets you edit or delete it. Deleting also deletes its history.

To change the order, drag an automation up or down by the dotted handle on its left.

**History** lists every run. Click one to see what happened: when it ran, how
long it took, its output and, for an agent, what was on its screen at the end.

| Status | What it means |
|---|---|
| succeeded | It worked. |
| failed | The command returned an error, the run took too long, or the agent quit before finishing. |
| blocked | The agent stopped to ask you something. Go to its space and answer. |
| closed | You closed the agent's space yourself. |
| skipped precheck | The precheck said there was nothing to do. |
| skipped overlap | The previous run was still going, so this one didn't start. |
| missed | The Mac was asleep or off at the scheduled time. |

## Asking your agent

The mod comes with a skill that teaches Claude, Codex and other agents how to
work with your automations. Once it's installed you can just ask things like:

- "Is my backup automation working? Why did it fail yesterday?"
- "Move the PR review to 8:00 and only run it on weekdays."
- "Pause everything until Monday."
- "Schedule an agent that cleans up stale branches every Friday."

The skill is linked into `~/.claude/skills` and `~/.agents/skills` when the mod
installs, unless you already have a skill with that name.

---

# Reference

The rest of this file is for when you want to edit the config by hand, use the
command line, or work on the mod itself.

## How it fits together

- **`automate-everything`**, a Rust program (`src/`), does the real work: it
  keeps the schedule, starts the runs, stores the config and the history, and
  has a command line for all of it. It runs as a background service (a launchd
  agent), which is why automations fire with the app closed.
- **The Automations panel** (`main.js` and the other `.js` files) is a view on
  top of that program. Anything you do in the panel goes through the command
  line, so the two always agree.

When the panel loads, and every 5 minutes after, it runs `install.sh`. That
downloads the released program for this version (or builds it, see Working
on the mod), copies it to
`~/.local/bin/automate-everything`, sets up and starts the background service,
and links the agent skill. Each step only runs when something changed. Runs in
progress keep going through an update. Install problems show in the panel; the
log is `.target/install.log`.

To uninstall the background service: `automate-everything service uninstall`.

Requirements: [docs/requirements.md](docs/requirements.md).

## Where things live

| What | Where |
|---|---|
| Config | `~/.config/automate-everything/automations.toml` |
| History, locks, logs | `~/.local/state/automate-everything/` (`runs/<id>/run.json`, `stdout.log`, `stderr.log`, `agent.log`) |
| Background service | `~/Library/LaunchAgents/com.lucasprag.automate-everything.plist` |
| Worktrees | `~/.herdr/worktrees/<repo>/automation/<name>/<stamp>` |

## Config file

One `[automations.<name>]` table per automation. You can edit the file by
hand; the service rereads it every minute, and `automate-everything check`
tells you if something is wrong. The panel and the command line only rewrite
the automation they change, so your comments and order stay put.

```toml
[settings]
retention_days = 30            # history ages out after this
max_runs_per_automation = 200

[automations.say-hello]
kind = "script"                # script | agent
schedule = "30 8-15 * * 1-5"   # cron, 5 fields; @hourly/@daily/@weekly too
timezone = "America/Toronto"   # default: this computer's
grace_minutes = 10             # a fire missed by more than this is recorded as missed
paused = false
command = "~/bin/hello --loud" # run by /bin/sh with your login environment
cwd = "~/Projects"             # default: home
timeout_seconds = 3600
env = { GREETING = "hi" }

[automations.nightly-review]
kind = "agent"
schedule = "0 7 * * 1-5"
repo = "~/Projects/smile/smile-core"
directory = "worktree"         # repo (default) | worktree: a new git worktree per run
base_branch = "origin/main"    # worktree only; fetched first
space = "per-run"              # per-run (default) | shared: tabs in one space named space_name
space_name = "Nightly review"  # default: the automation's name
agent = "claude"               # default claude
model = "opus"                 # passed as --model
agent_args = ["--effort", "high"]
prompt = """Review yesterday's merged PRs..."""   # or prompt_file = "~/prompts/review.md"
precheck = "~/bin/anything-to-review"   # scheduled runs only: exit 0 runs, anything else skips
precheck_timeout_seconds = 120
timeout_seconds = 10800
```

Every run gets the environment variables `HERDR_AUTOMATION`,
`HERDR_AUTOMATION_RUN_ID` and `HERDR_AUTOMATION_TRIGGER` (`scheduled` or
`manual`).

## How runs behave, in detail

- **Scripts** run in the background with no space. Output and exit code go into
  the run's record.
- **Agent runs** open a terminal through herdr-core, labelled with Herdr's space
  and tab ids, so they show up as a space (or a tab in the shared space) even
  when the window is closed. The agent runs inside a login shell that stays
  after the agent ends, so a failed run's space still shows what happened.
- **Finished means finished:** the agent has to take the prompt (its first turn
  starts, or it shows as working), then report done or idle with its process
  still alive for about 10 seconds.
- **Success closes what the run opened:** its space, if the run made it and
  every terminal there belongs to this run (`automation.run=<id>` label);
  otherwise only its tab.
- **Failure stays open.** An agent that never takes the prompt within 3 minutes
  has its space closed. An agent that exits early or hits its timeout leaves
  the space open (the agent is stopped, its shell stays).
- **Blocked hands the run to you:** no timeout and no auto-close. It ends as
  succeeded when the agent finishes (the space stays open, since you stepped
  in) or closed when you close the space.
- **Worktrees** are removed with `git worktree remove` once the run's space is
  gone, including spaces you close by hand. The branch is deleted only if it
  has no new commits or they're merged or pushed; otherwise the record says
  why it was kept.
- **Claude's folder-trust question** shows up for every new worktree. For the
  worktree the run just made from the configured repo, and only that one, the
  runner answers yes and notes it in the record.
- **One run at a time** per automation, held by a lock for as long as the run
  lasts, a blocked one included. A scheduled time that comes up meanwhile is
  recorded as skipped overlap.
- **Catching up after sleep:** the newest missed time runs once if it's inside
  the grace window; otherwise one missed record covers them all. Daylight
  saving: a fixed-time automation runs once when clocks go back and isn't lost
  when they go forward.

## Command line

```
automate-everything list | run <name> [--wait] | pause <name> | resume <name> | delete <name>
automate-everything move <name> --to <position>
automate-everything history [<name>] | show <run-id>
automate-everything get <name> | set <name> --fields '<json>' [--create] | check
automate-everything status | service install|uninstall|restart | scheduler
```

Add `--json` to `list`, `history` and `show` for machine-readable output.

### Extra tabs from an agent run

An agent run can open more tabs in its own space, for a helper agent or a shell
in another worktree, and close them when they're done:

```
automate-everything tab open --name <tab> --cwd <dir> [--label k=v] [--trust] [--wait-start] [-- <program> <args>]
automate-everything tab close --label k=v [--except-self] | tab close --self
```

`--trust` answers Claude's folder-trust question for `--cwd` only.
`--wait-start` waits until the agent has taken its prompt. `tab close` only
closes tabs opened with `tab open`, never the run's own tab or anything you
opened. When the run succeeds it closes its own tab; the tabs it opened stay
until they close themselves, and the space closes with its last tab.

## Panel keys

`keys.json` can send `automate-everything.history`, `.home`, `.new`,
`.edit:<name>`, `.menu:<name>` and `.run:<run-id>` to the panel.

## Working on the mod

`install.sh` downloads the released program only when this folder holds
exactly the released source: the tag for `Cargo.toml`'s version exists and
`src/`, `Cargo.toml` and `Cargo.lock` haven't changed since. Otherwise it
builds with `cargo` ([rustup.rs](https://rustup.rs)), so your own changes are
what runs. A download is checked against its `.sha256`; a private repo needs
`gh` signed in.

To release, bump `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock`
follows, commit, then tag and push:

```sh
git tag v0.2.0 && git push origin main v0.2.0
```

The release workflow (`.github/workflows/release.yml`) checks the tag matches
the version, builds for Apple Silicon and Intel, joins them into one universal
binary and attaches it and its checksum to a GitHub release. Everyone's panel
picks it up within 5 minutes of them pulling.

`cargo test` covers cron, daylight saving and config parsing. Point a test at
a sandbox with `AUTOMATE_EVERYTHING_CONFIG` and `AUTOMATE_EVERYTHING_STATE`.
Check the mod with `herdr-app check ~/.config/herdr-app/mods/automate-everything`.
