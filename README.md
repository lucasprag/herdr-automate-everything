# automate-everything

Scheduled scripts and agent runs for the Herdr desktop app. Requirements:
[docs/requirements.md](docs/requirements.md).

Two parts:

- **`automate-everything`**, a Rust binary (`src/`): the scheduler (a launchd
  agent, so it fires with the app closed), the runner, the config file and the
  history, and a CLI over all of it.
- **This mod** (`main.js` and friends): the Automations panel in the side
  panel. It only reads and acts through the CLI.

## Install

Nothing to do by hand. When the Automations panel loads (and every 5 minutes)
it runs `install.sh` in the background, which:

1. rebuilds the binary if `src/` changed (needs `cargo`),
2. copies it to `~/.local/bin/automate-everything` if it differs,
3. writes the launchd agent and starts it if it's missing or out of date.

Each step runs only when something changed. A running scheduler notices a new
binary within a minute and exits; launchd starts the new one, and runs in
progress keep going. Problems show in the panel's banner; the log is
`.target/install.log`. To remove it: `automate-everything service uninstall`.

## Where things live

| What | Where |
|---|---|
| Config | `~/.config/automate-everything/automations.toml` |
| History, locks, logs | `~/.local/state/automate-everything/` (`runs/<id>/run.json`, `stdout.log`, `stderr.log`, `agent.log`) |
| launchd agent | `~/Library/LaunchAgents/com.lucasprag.automate-everything.plist` |
| Worktrees | `~/.herdr/worktrees/<repo>/automation/<name>/<stamp>` |

## Config

One `[automations.<name>]` table per entry. The panel and the CLI rewrite only
the entry they change; comments, order and the other entries stay as written.

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

Every run gets `HERDR_AUTOMATION`, `HERDR_AUTOMATION_RUN_ID` and
`HERDR_AUTOMATION_TRIGGER` (`scheduled` or `manual`).

## How runs behave

- **Scripts** run headless: no agent, no space. Output and exit code go into the run's record.
- **Agent runs** open a terminal through herdr-core labelled with Herdr's space
  and tab ids, so they show up as a space (or a tab in the shared space) even
  when the window is closed. The agent runs inside a login shell that stays
  after the agent ends, so a failed run's space still shows what happened.
- **Done is proved:** the agent has to take the prompt (turn ≥ 1 or working),
  then report `done`/`idle` with its process alive for about 10 s.
- **Success closes what the run opened:** its space if it made it and every
  terminal there is this run's (`automation.run=<id>` label), else only its tab.
- **Failure stays open:** never taking the prompt (3 min) closes what it opened;
  an agent that exits early or hits its timeout (the agent is stopped, not its
  shell) leaves the space open.
- **Blocked hands the run to you:** no timeout and no auto-close. It ends as
  `succeeded` when the agent finishes (the space stays open, since you stepped
  in) or `closed` when you close the space.
- **Closed by hand** is recorded as `closed`, not a failure.
- **Worktrees** are removed with plain `git worktree remove` once the run's space
  is gone (the scheduler checks every minute for spaces closed by hand). The
  branch is deleted only if it has no new commits or they're merged or pushed;
  otherwise the record says why it was kept.
- **Claude's folder-trust question** shows for every new worktree. For the
  worktree the run just made from the configured repo, and only that, the
  runner answers yes, and the record notes it.
- **Overlap:** one run per automation at a time, held by a kernel lock for as
  long as the run lasts (a blocked one included). A fire meanwhile is recorded as `skipped_overlap`.
- **Catch-up:** after sleep, the newest missed fire runs once if it's inside the
  grace window; otherwise one `missed` record covers them. DST: a fixed-time
  entry fires once when clocks go back and isn't lost when they go forward.

## CLI

```
automate-everything list | run <name> [--wait] | pause <name> | resume <name> | delete <name>
automate-everything history [<name>] | show <run-id>
automate-everything get <name> | set <name> --fields '<json>' [--create] | check
automate-everything status | service install|uninstall|restart | scheduler
```

## Panel keys

`keys.json` can send `automate-everything.history`, `.home`, `.new`,
`.edit:<name>`, `.menu:<name>` and `.run:<run-id>` to the panel.

## Develop

`cargo test` covers cron, DST and config parsing. Point a test at a sandbox
with `AUTOMATE_EVERYTHING_CONFIG` and `AUTOMATE_EVERYTHING_STATE`. Check the
mod with `herdr-app check ~/.config/herdr-app/mods/automate-everything`.
