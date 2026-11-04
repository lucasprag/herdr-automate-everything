#!/bin/sh
# Brings the installed copy up to date with this folder: downloads the
# released binary for this version (or builds it when src/ has changed since
# the release), then `service ensure` copies it to ~/.local/bin and makes
# sure the launchd agent is written and running. Each step runs only when
# something changed, so this is cheap to repeat. It also links the agent skill
# (skills/automate-everything) into ~/.agents/skills and ~/.claude/skills.
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

# The panel runs this with a short PATH; add where gh and cargo usually live.
PATH="$PATH:/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
tag="v$version"
repo=$(git remote get-url origin 2>/dev/null | sed -n 's#.*github\.com[:/]\(.*\)$#\1#p' | sed 's#\.git$##')
repo=${repo:-lucasprag/herdr-automate-everything}

# The released binary is used when this folder holds exactly the released
# source: the version's tag exists and src/ hasn't changed since. A copy that
# isn't a git checkout trusts its version. Anything else is built.
released=no
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null 2>&1; then
  git diff --quiet "$tag" -- src Cargo.toml Cargo.lock 2>/dev/null && released=yes
elif ! git rev-parse --git-dir >/dev/null 2>&1; then
  released=yes
fi

# Downloads the release's universal binary and checks it against its
# checksum. curl works for a public repo; gh (signed in) for a private one.
download() {
  dir=.target/dist
  asset=automate-everything-macos
  rm -rf "$dir/.new" && mkdir -p "$dir/.new" || return 1
  url="https://github.com/$repo/releases/download/$tag/$asset"
  if ! { curl -fsSL -o "$dir/.new/$asset" "$url" && curl -fsSL -o "$dir/.new/$asset.sha256" "$url.sha256"; } 2>/dev/null; then
    command -v gh >/dev/null 2>&1 \
      && gh release download "$tag" -R "$repo" -p "$asset" -p "$asset.sha256" -D "$dir/.new" --clobber >/dev/null 2>&1 \
      || return 1
  fi
  want=$(awk '{print $1}' "$dir/.new/$asset.sha256")
  got=$(shasum -a 256 "$dir/.new/$asset" | awk '{print $1}')
  [ -n "$want" ] && [ "$want" = "$got" ] || { echo "the download of $tag didn't match its checksum"; return 1; }
  chmod +x "$dir/.new/$asset" && mv "$dir/.new/$asset" "$1" && rm -rf "$dir/.new"
}

bin=""
if [ "$released" = yes ]; then
  dl=.target/dist/automate-everything-$version
  if [ -x "$dl" ] || { echo "downloading ${tag}…"; download "$dl"; }; then
    bin=$dl
  else
    echo "couldn't download $tag from $repo"
  fi
fi
if [ -z "$bin" ]; then
  bin=.target/release/automate-everything
  if [ ! -x "$bin" ] || [ -n "$(find src Cargo.toml Cargo.lock -newer "$bin" 2>/dev/null | head -1)" ]; then
    if ! command -v cargo >/dev/null 2>&1; then
      [ "$released" = yes ] && fail "couldn't download $tag, and cargo isn't installed to build it"
      fail "src/ differs from a release, and cargo isn't installed to build it"
    fi
    echo "building…"
    cargo build --release || fail "the build failed"
  fi
fi
"$bin" service ensure || fail "installing failed"

# The agent skill: link it where agents look for skills, unless something is
# already there under that name.
for dir in "$HOME/.agents/skills" "$HOME/.claude/skills"; do
  link="$dir/automate-everything"
  if [ -d "$dir" ] && [ ! -e "$link" ] && [ ! -L "$link" ]; then
    ln -s "$PWD/skills/automate-everything" "$link" && echo "linked the skill into $dir"
  fi
done
echo 0 > "$status"
