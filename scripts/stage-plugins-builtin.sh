#!/usr/bin/env bash
# Zeron's own plugins into a staged bundle's plugins-builtin/: the app's own
# (apps/zgx/plugins-builtin/) and the repository's plugins (the Captains dsh
# bundle) beside them, each Luau plugin handed the canonical SDK. All three
# package scripts share this one step, so the bundle's layout is written once:
#
#   scripts/stage-plugins-builtin.sh <dir>
#
# stages into <dir>/plugins-builtin/ — the tarball's root on Linux, Zeron.app's
# Contents/Resources on macOS, beta\<version> on Windows. package-windows.ps1
# calls this through Git Bash, so cp and find only: Git Bash has no rsync.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${1:?usage: scripts/stage-plugins-builtin.sh <staged dir>}/plugins-builtin"
mkdir -p "$DEST"
for src in "$ROOT/apps/zgx/plugins-builtin/"*/ "$ROOT/plugins/dsh-captains"; do
  cp -R "$src" "$DEST/"
done
# Dev-machine leftovers stay out, as the rsync and robocopy copies kept them out.
find "$DEST" -depth \( -name '.zgx' -o -name '.DS_Store' -o -name '.lock' \) -exec rm -rf {} +
# Each Luau plugin (a folder with a manifest) ships the canonical SDK, whatever
# copy sits in its folder; a dsh bundle like the Captains plugin is not one.
for dir in "$DEST"/*/; do
  [[ -f "${dir}zeron-plugin.json" ]] || continue
  cp "$ROOT/apps/zgx/plugins-sdk/zeron.luau" "${dir}zeron.luau"
done
