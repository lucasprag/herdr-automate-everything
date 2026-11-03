#!/bin/sh
# Brings the installed copy up to date with this folder: rebuilds the binary
# when src/ changed, then `service ensure` copies it to ~/.local/bin and makes
# sure the launchd agent is written and running. Each step runs only when
# something changed, so this is cheap to repeat.
#
# Nothing to do by hand: the Automations panel runs this when it loads and
# every few minutes (log: .target/install.log, result: .target/install.status).
set -u
cd "$(dirname "$0")"
mkdir -p .target
status=.target/install.status
lock=.target/install.lock

# One at a time; a lock older than 15 minutes is a leftover.
if ! mkdir "$lock" 2>/dev/null; then
  if [ -n "$(find "$lock" -maxdepth 0 -mmin +15 2>/dev/null)" ]; then
    rmdir "$lock" && mkdir "$lock" || exit 0
  else
    exit 0
  fi
fi
trap 'rmdir "$lock" 2>/dev/null' EXIT

fail() { echo "$1"; echo 1 > "$status"; exit 1; }

bin=.target/release/automate-everything
if [ ! -x "$bin" ] || [ -n "$(find src Cargo.toml Cargo.lock -newer "$bin" 2>/dev/null | head -1)" ]; then
  command -v cargo >/dev/null 2>&1 || fail "cargo isn't installed, so the binary can't be built"
  echo "building…"
  cargo build --release || fail "the build failed"
fi
"$bin" service ensure || fail "installing failed"
echo 0 > "$status"
