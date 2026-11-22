#!/usr/bin/env bash
# SUR-152: a red pull-request leg cancels the run, so the legs still running
# stop at once instead of finishing for a verdict already known — but only
# when the run's head is superseded (SUR-173): on the live head a cancel
# would throw the failure's evidence away with the run.
# SUR-163/SUR-173: before any cancel, an ::error annotation names the failed
# step and carries an excerpt of its output; the job summary keeps the same
# excerpt in a fenced block. A pull-request reader sees the error itself,
# not a pointer into a log the cancel may have deleted.
#
# The excerpt comes from $RUNNER_TEMP/first-red.log (FIRST_RED_LOG overrides
# it, for tests): a job's own log is not downloadable from the API while the
# job still runs, so each build/test step tees its output there under a
# "first-red-step: <name>" marker line (the workflows' `exec > >(tee -a …)`
# and PowerShell `Start-Transcript` wiring keeps every exit status). When
# the file or the step's marker is missing — checkout failed, an unwired
# step — the annotation falls back to the step name alone, as before.
#
# Supersession is decided from the pull request itself. The run's head is the
# event's .pull_request.head.sha ($GITHUB_SHA is the merge ref, which moves
# whenever main does); the live head is `gh api repos/…/pulls/N --jq
# .head.sha`. Different → publish, then cancel. Same → publish only. Unknown
# — no PR payload, no jq, an API error — → publish only: a cancel that
# cannot be justified must not happen.
set -uo pipefail

repo="${GITHUB_REPOSITORY:?}"
run="${GITHUB_RUN_ID:?}"
job="${GITHUB_JOB:-unknown}"
runner="${RUNNER_NAME:-}"
log="${FIRST_RED_LOG:-${RUNNER_TEMP:-}/first-red.log}"

# --- Which step failed ----------------------------------------------------
# The runner's name names this job (one job runs on a runner at a time), and
# its steps say which one failed — and whether anything did. "green" when
# every step is concluded and none failed, or when only the last step — this
# one, the workflows' "First red …" name — is still running: invoked with
# nothing red, there is nothing to publish. "pending" while the API shows an
# earlier step still running (it lags the runner's report by a beat; the
# retry covers it); pending past the retries falls back to the job-only
# annotation, as before.
step=""
for _ in 1 2 3; do
  status="$(gh api "repos/${repo}/actions/runs/${run}/jobs?per_page=100" \
    --jq "([.jobs[] | select(.runner_name == \"${runner}\" and .status == \"in_progress\")][0].steps // [])
          | if length == 0 then \"pending\"
            elif any(.[]; .conclusion == \"failure\") then \"failed: \" + ([.[] | select(.conclusion == \"failure\")][0].name // \"\")
            elif any(.[:-1][]; .conclusion == null) then \"pending\"
            elif .[-1].conclusion == null and ((.[-1].name // \"\") | startswith(\"First red\") | not) then \"pending\"
            else \"green\" end" \
    2>/dev/null || true)"
  case "$status" in
    green) exit 0 ;;
    failed:*)
      step="${status#failed:}"
      step="${step# }"
      break ;;
    *) sleep "${FIRST_RED_RETRY_DELAY:-2}" ;;
  esac
done

# --- The failed step's excerpt --------------------------------------------
# The last window worth reading: the section the step marked for itself when
# it has one, the last error-ish line (plus a little of what followed), and
# never more than ~60 lines / ~3.5 KB — all of it must fit the annotation
# and the summary gets the same text. python3 does the picking, then a plain
# tail if that is somehow not there.
excerpt=""
if [ -s "$log" ]; then
  if command -v python3 >/dev/null 2>&1; then
    excerpt="$(python3 - "$log" "$step" <<'PY' 2>/dev/null || true
import re
import sys

log_path, step = sys.argv[1], sys.argv[2]
MARKER = "first-red-step: "
MAX_BACK, MAX_LINES, MAX_CHARS = 300_000, 60, 3500

with open(log_path, "rb") as f:
    f.seek(0, 2)
    f.seek(max(0, f.tell() - MAX_BACK))
    text = f.read().decode("utf-8", "replace")
# The tee'd copy may carry ANSI colour and CRs; neither helps the annotation.
text = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", text).replace("\r", "")
lines = text.split("\n")

# Each step's output sits between its marker and the next one's; the failed
# step's own section is the excerpt's pool, the whole tail when it left none.
sections, name, cur = [], "", []
for line in lines:
    if line.startswith(MARKER):
        sections.append((name, cur))
        name, cur = line[len(MARKER):].strip(), []
    else:
        cur.append(line)
sections.append((name, cur))

pool = [l for l in lines if not l.startswith(MARKER)]
if step:
    for name, section in sections:
        if name == step:
            pool = section

hits = [i for i, l in enumerate(pool) if re.search(r"error|fail|panic|assert", l, re.I)]
end = min(len(pool), hits[-1] + 12) if hits else len(pool)
start = max(0, end - MAX_LINES)
out = "\n".join(pool[start:end])
truncated = start > 0
if len(out) > MAX_CHARS:
    out = out[-MAX_CHARS:]
    nl = out.find("\n")
    if 0 <= nl < len(out) - 1:
        out = out[nl + 1:]
    truncated = True
print(("…\n" if truncated else "") + out)
PY
)"
  fi
  [ -n "$excerpt" ] || excerpt="$(tail -n 60 "$log" 2>/dev/null || true)"
fi

# --- Publish: the annotation, then the job summary -------------------------
esc_data() {
  local s="${1//'%'/%25}"
  s="${s//$'\r'/%0D}"
  s="${s//$'\n'/%0A}"
  printf '%s' "$s"
}
esc_prop() {
  local s
  s="$(esc_data "$1")"
  s="${s//:/%3A}"
  printf '%s' "${s//,/%2C}"
}

if [ -n "$step" ]; then
  title="First red: ${job} / ${step}"
  line="${job} failed at step \"${step}\""
else
  title="First red"
  line="${job} failed"
fi
body="$line"
[ -n "$excerpt" ] && body="${body}"$'\n\n'"${excerpt}"
echo "::error title=$(esc_prop "$title")::$(esc_data "$body")"

summary="${GITHUB_STEP_SUMMARY:-}"
if [ -n "$summary" ]; then
  {
    printf '### First red: `%s`' "$job"
    [ -n "$step" ] && printf ' — step `%s`' "$step"
    printf '\n'
    if [ -n "$excerpt" ]; then
      fence='```'
      while printf '%s\n' "$excerpt" | grep -qF "$fence"; do fence="${fence}\`"; done
      printf '\n%s\n%s\n%s\n' "$fence" "$excerpt" "$fence"
    fi
  } >> "$summary" 2>/dev/null || true
fi

# --- Superseded head? publish first, cancel second --------------------------
pr="" head="" live=""
event="${GITHUB_EVENT_PATH:-}"
if [ -n "$event" ] && [ -f "$event" ] && command -v jq >/dev/null 2>&1; then
  pr="$(jq -r '.pull_request.number // empty' "$event" 2>/dev/null || true)"
  head="$(jq -r '.pull_request.head.sha // empty' "$event" 2>/dev/null || true)"
fi
if [ -n "$pr" ] && [ -n "$head" ]; then
  live="$(gh api "repos/${repo}/pulls/${pr}" --jq .head.sha 2>/dev/null || true)"
fi
if [ -n "$live" ] && [ "$live" != "$head" ]; then
  echo "::notice title=First red::PR #${pr}'s head moved on (${head} -> ${live}); cancelling the run"
  gh api --method POST "repos/${repo}/actions/runs/${run}/cancel" || true
elif [ -n "$live" ]; then
  echo "first-red: PR #${pr}'s head ${head} is still live; the run keeps its evidence"
else
  echo "first-red: PR head unknown; the run is left alone"
fi
exit 0
