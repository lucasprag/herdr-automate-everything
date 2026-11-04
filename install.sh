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

# Which binary runs:
# 1. This version's release, when this folder holds exactly the released
#    source (the version's tag exists and src/ hasn't changed since; a copy
#    that isn't a git checkout trusts its version). No Rust needed.
# 2. Otherwise a build of src/ with cargo, so changes run without a release.
# 3. Without cargo, the latest release: src/ has unreleased changes, but the
#    mod still works for someone who never installed Rust.
released=no
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null 2>&1; then
  git diff --quiet "$tag" -- src Cargo.toml Cargo.lock 2>/dev/null && released=yes
elif ! git rev-parse --git-dir >/dev/null 2>&1; then
  released=yes
fi

# download <tag> <dest>: the release's universal binary, checked against its
# checksum. curl works for a public repo; gh (signed in) for a private one.
download() {
  dir=.target/dist
  asset=automate-everything-macos
  rm -rf "$dir/.new" && mkdir -p "$dir/.new" || return 1
  url="https://github.com/$repo/releases/download/$1/$asset"
  if ! { curl -fsSL -o "$dir/.new/$asset" "$url" && curl -fsSL -o "$dir/.new/$asset.sha256" "$url.sha256"; } 2>/dev/null; then
    command -v gh >/dev/null 2>&1 \
      && gh release download "$1" -R "$repo" -p "$asset" -p "$asset.sha256" -D "$dir/.new" --clobber >/dev/null 2>&1 \
      || return 1
  fi
  want=$(awk '{print $1}' "$dir/.new/$asset.sha256")
  got=$(shasum -a 256 "$dir/.new/$asset" | awk '{print $1}')
  [ -n "$want" ] && [ "$want" = "$got" ] || { echo "the download of $1 didn't match its checksum"; return 1; }
  chmod +x "$dir/.new/$asset" && mv "$dir/.new/$asset" "$2" && rm -rf "$dir/.new"
}

# get <tag>: that release's binary, downloaded once; prints its path.
get() {
  dest=.target/dist/automate-everything-${1#v}
  if [ ! -x "$dest" ]; then
    echo "downloading ${1}…" >&2
    download "$1" "$dest" >&2 || return 1
  fi
  echo "$dest"
}

latest_tag() {
  t=$(curl -fsSI "https://github.com/$repo/releases/latest" 2>/dev/null | tr -d '\r' | sed -n 's#^[Ll]ocation: .*/tag/##p')
  if [ -z "$t" ] && command -v gh >/dev/null 2>&1; then
    t=$(gh release view -R "$repo" --json tagName -q .tagName 2>/dev/null)
  fi
  echo "$t"
}

bin=""
if [ "$released" = yes ]; then
  bin=$(get "$tag") || { bin=""; echo "couldn't download $tag from $repo"; }
fi
if [ -z "$bin" ] && command -v cargo >/dev/null 2>&1; then
  bin=.target/release/automate-everything
  if [ ! -x "$bin" ] || [ -n "$(find src Cargo.toml Cargo.lock -newer "$bin" 2>/dev/null | head -1)" ]; then
    echo "building…"
    cargo build --release || fail "the build failed"
  fi
fi
if [ -z "$bin" ]; then
  latest=$(latest_tag)
  if [ -n "$latest" ] && bin=$(get "$latest"); then
    [ "$released" = no ] && echo "running $latest: src/ has changes since, which need Rust (rustup.rs) to build"
  else
    # Offline: the newest release downloaded before, if any.
    bin=$(ls -t .target/dist/automate-everything-* 2>/dev/null | head -1)
    [ -n "$bin" ] || fail "couldn't download a release from $repo, and cargo isn't installed to build one"
    echo "offline: running $bin"
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
