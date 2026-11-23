#!/usr/bin/env bash
# Zeron's own plugins into a staged bundle's plugins-builtin/, and the
# Captains dsh bundle beside them in dsh-plugins/ — outside plugins-builtin/,
# the set the app's `SetBuiltinPlugins` scans as Zeron's own: every folder
# there shows as one built-in, and one without a zeron-plugin.json as a
# broken, unremovable row. All three package scripts share this one step, so
# the bundle's layout is written once:
#
#   scripts/stage-plugins-builtin.sh <dir>
#
# stages into <dir>/plugins-builtin/ and <dir>/dsh-plugins/ — the tarball's
# root on Linux, Zeron.app's Contents/Resources on macOS, beta\<version> on
# Windows. package-windows.ps1 calls this through Git Bash, so cp and find
# only: Git Bash has no rsync.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAGE="${1:?usage: scripts/stage-plugins-builtin.sh <staged dir>}"
DEST="$STAGE/plugins-builtin"
mkdir -p "$DEST"
for src in "$ROOT/apps/zgx/plugins-builtin/"*/; do
  cp -R "$src" "$DEST/"
done
# The Captains dsh bundle, by name only: the manifest, the patch, the entry
# and the runtime source — no test/, no *.test.mjs, no Luau SDK copy.
DSH="$STAGE/dsh-plugins/dsh-captains"
mkdir -p "$DSH"
cp "$ROOT/plugins/dsh-captains/package.json" "$ROOT/plugins/dsh-captains/cordis.patch.yml" \
  "$ROOT/plugins/dsh-captains/index.js" "$DSH/"
cp -R "$ROOT/plugins/dsh-captains/src" "$DSH/src"
# Dev-machine leftovers stay out, as the rsync and robocopy copies kept them out.
find "$DEST" "$DSH" -depth \( -name '.zgx' -o -name '.DS_Store' -o -name '.lock' \) -exec rm -rf {} +
# Each Luau plugin (a folder with a manifest) ships the canonical SDK, whatever
# copy sits in its folder; a dsh bundle like the Captains plugin is not one.
for dir in "$DEST"/*/; do
  [[ -f "${dir}zeron-plugin.json" ]] || continue
  cp "$ROOT/apps/zgx/plugins-sdk/zeron.luau" "${dir}zeron.luau"
done
