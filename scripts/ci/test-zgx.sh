#!/usr/bin/env bash
# The apps/zgx tests CI runs (.github/workflows/zgx-tests.yml), with the known
# failures in scripts/ci/zgx-known-failures.txt let through:
#
#   scripts/ci/test-zgx.sh rust [n]   cargo test --bin zeron-zgx (its unit tests,
#                                     src/plugins/*_tests.rs among them), the
#                                     known ones skipped by exact name, and the
#                                     `rust-macos:` ones on the macOS runner
#                                     only. With n, the Linux job's shard (1 or
#                                     2, SUR-174): shard 1 runs the modules in
#                                     SHARD1, shard 2 every other test.
#   scripts/ci/test-zgx.sh tests      cargo test --test '*' (apps/zgx/tests/*.rs,
#                                     the app's integration tests), every binary
#                                     run however others fail (--no-fail-fast),
#                                     the known ones skipped by exact name
#   scripts/ci/test-zgx.sh ui [zgx]   zgx test ui (the Luau UI tests), with the
#                                     zgx CLI given or on PATH; fails on any FAIL
#                                     not listed, and names listed ones that pass
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
known="$root/scripts/ci/zgx-known-failures.txt"

listed() { sed -n "s/^$1: //p" "$known"; }

# The unit tests' shard 1 (SUR-174), split by measured duration, not count.
# transcript_paint_tests is ~90 s on its own: each test repaints a whole
# transcript on lavapipe behind a static GPU mutex, so the module is one long
# serial chain. Everything else parallelizes over the runner's cores and sums
# to about the same wall time (measured 87 s vs 100 s at four threads), so the
# shard holds only 6 tests of 789. Pairing the paints with anything bigger
# made the shard slower both ways: they inflate whatever shares their process.
# Shard 2 is everything not here, so a test added anywhere, a new module among
# them, always lands in a shard. The names are libtest path prefixes.
SHARD1=(
  transcript_paint_tests:: # 6 tests, ~90 s serial: whole transcripts repainted on lavapipe
)

case "${1:-}" in
  rust)
    skips=()
    skip_listed() {
      local name
      while IFS= read -r name; do
        if [ -n "$name" ]; then skips+=(--skip "$name"); fi
      done < <(listed "$1")
    }
    skip_listed rust
    # (A test the macOS runner can't pass, which the other runners keep running.)
    if [ "$(uname -s)" = Darwin ]; then skip_listed rust-macos; fi
    cd "$root/apps/zgx"
    shard="${2:-}"
    if [ -n "$shard" ]; then
      # Every test the binary lists, by exact name: a prefix that matches
      # nothing warns (a renamed module would drift to shard 2 unnoticed).
      all=()
      while IFS= read -r name; do all+=("$name"); done \
        < <(cargo test --locked --bin zeron-zgx -- --list | sed -n 's/: test$//p')
      one=()
      seen=" "
      for name in "${all[@]}"; do
        for prefix in "${SHARD1[@]}"; do
          case "$name" in
            "$prefix"*) one+=("$name"); seen="$seen$prefix "; break ;;
          esac
        done
      done
      for prefix in "${SHARD1[@]}"; do
        case "$seen" in *"$prefix "*) ;; *) echo "::warning::shard 1 prefix $prefix matches no test" >&2 ;; esac
      done
      case "$shard" in
        1)
          # No filters would run everything; an empty list is a stale SHARD1.
          if [ ${#one[@]} -eq 0 ]; then
            echo "::error::shard 1 matched no test; fix SHARD1 in $0" >&2; exit 1
          fi
          echo "unit-test shard 1: ${#one[@]} of ${#all[@]} tests"
          exec cargo test --locked --bin zeron-zgx -- --exact ${one[@]+"${one[@]}"} ${skips[@]+"${skips[@]}"}
          ;;
        2)
          args=()
          for name in ${one[@]+"${one[@]}"}; do args+=(--skip "$name"); done
          echo "unit-test shard 2: $((${#all[@]} - ${#one[@]})) of ${#all[@]} tests"
          exec cargo test --locked --bin zeron-zgx -- --exact ${args[@]+"${args[@]}"} ${skips[@]+"${skips[@]}"}
          ;;
        *) echo "usage: $0 rust [1|2] | tests | ui [zgx]" >&2; exit 2 ;;
      esac
    fi
    # (`${a[@]+…}`: an empty list under `set -u`, which macOS's bash 3.2
    # calls unbound.)
    exec cargo test --locked --bin zeron-zgx -- --exact ${skips[@]+"${skips[@]}"}
    ;;
  tests)
    skips=()
    while IFS= read -r name; do skips+=(--skip "$name"); done < <(listed tests)
    cd "$root/apps/zgx"
    exec cargo test --locked --no-fail-fast --test '*' -- --exact ${skips[@]+"${skips[@]}"}
    ;;
  ui)
    zgx="${2:-zgx}"
    log="$(mktemp)"
    cd "$root/apps/zgx"
    status=0
    "$zgx" test ui 2>&1 | tee "$log" | grep -vE '^warning: Luau was built with its asserts' || status=$?
    # `FAIL <name> (<file>)`
    failed="$(sed -n 's/^FAIL \(.*\) ([^()]*)$/\1/p' "$log" | sort -u)"
    expected="$(listed luau | sort -u)"
    new="$(comm -23 <(printf '%s\n' "$failed") <(printf '%s\n' "$expected") | sed '/^$/d')"
    fixed="$(comm -13 <(printf '%s\n' "$failed") <(printf '%s\n' "$expected") | sed '/^$/d')"
    ran="$(grep -cE '^(ok|FAIL) ' "$log" || true)"
    echo "zgx test ui: $ran tests, $(printf '%s' "$failed" | grep -c . || true) failed"
    if [ -n "$fixed" ]; then
      echo "::notice::known zgx UI failures that passed (delete them from scripts/ci/zgx-known-failures.txt):"
      printf '  %s\n' "$fixed"
    fi
    if [ -n "$new" ]; then
      echo "::error::zgx UI tests that failed and aren't known failures:"
      printf '  %s\n' "$new"
      exit 1
    fi
    # Failed for something other than tests (no tests ran, a crash).
    if [ "$ran" -eq 0 ] || { [ "$status" -ne 0 ] && [ -z "$failed" ]; }; then
      echo "::error::zgx test ui exited $status with no test failures to show for it"
      exit 1
    fi
    ;;
  *)
    echo "usage: $0 rust [1|2] | tests | ui [zgx]" >&2
    exit 2
    ;;
esac
