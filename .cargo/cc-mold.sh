#!/bin/sh
# cc driver that links with mold when it is installed (clang always accepts
# -fuse-ld with an absolute path) and falls back to plain cc when it is not, so
# committing it is safe everywhere. The final link of this workspace's test
# binaries is a large part of every incremental build; mold cuts it several
# times (apt/dnf/pacman: mold).
if command -v mold >/dev/null 2>&1 && command -v clang >/dev/null 2>&1; then
    exec clang -fuse-ld="$(command -v mold)" "$@"
fi
exec cc "$@"
