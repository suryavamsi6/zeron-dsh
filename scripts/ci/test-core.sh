#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

tests=()
for f in crates/harness/tests/*.rs crates/preview/tests/*.rs crates/doc/tests/*.rs; do
  [[ -f "$f" ]] || continue
  tests+=(--test "$(basename "$f" .rs)")
done
for t in e2e big_remote_chats warm_start misc geometry session_publication restart_resume side_chats message_queue device_routing device_room plugin_services restart_hook captain_role_runtime captains_profile_provisioning; do
  tests+=(--test "$t")
done

exec cargo nextest run --config-file scripts/ci/nextest.toml --locked --no-fail-fast \
  -p zeron-harness -p zeron-engine -p zeron-sync -p zeron-update -p zeron-doc -p zeron-preview \
  -p zeron-mcp -p zeron-proto -p zeron-rpc -p zeron-orb -p zeron-voice-session \
  -p zeron-cli -p zeron-launch -p zeron-notify \
  --lib --bins "${tests[@]}" \
  -E 'not (package(zeron-rpc) & kind(lib))' "$@"
