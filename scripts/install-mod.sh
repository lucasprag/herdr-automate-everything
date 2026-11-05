#!/bin/sh
# Copies the mod into the Herdr app's home and records this checkout, so the
# mod's install.sh builds your changes to src/ instead of downloading a
# release. A copy, not a symlink: Herdr doesn't reload a mod behind a symlink.
#
# Run it after cloning and after every pull or change to app/mods/.
set -eu
repo="$(cd "$(dirname "$0")/.." && pwd)"
src="$repo/app/mods/automate-everything"
dst="${HERDR_APP_HOME:-$HOME/.config/herdr-app}/mods/automate-everything"
[ -L "$dst" ] && rm "$dst"
mkdir -p "$dst/.target"
# --delete keeps the copy exact; .target (downloads, install log) and the
# files Herdr writes into every mod stay.
rsync -a --delete \
  --exclude .target --exclude gpui-kit.d.ts --exclude herdr.d.ts --exclude jsconfig.json \
  "$src/" "$dst/"
echo "$repo" > "$dst/.target/source"
herdr-app check "$dst"
