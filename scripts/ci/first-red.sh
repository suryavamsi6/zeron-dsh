#!/usr/bin/env bash
# SUR-163: the first-red annotation names the step that failed, not only its
# job — once the run is cancelled, that step survives only in the job's log.
# SUR-152: a red pull-request leg cancels the run, so the legs still running
# stop at once instead of finishing for a verdict already known. The
# annotation must go first: the cancel takes the failing job down with it.
set -uo pipefail

repo="${GITHUB_REPOSITORY:?}"
run="${GITHUB_RUN_ID:?}"
job="${GITHUB_JOB:-unknown}"
runner="${RUNNER_NAME:-}"

# Read this job from the run's jobs: one job runs on a runner at a time, so
# the runner's name names it, and the job's own steps say which one failed.
# A short retry covers the API lagging the runner's report; if it still says
# nothing, the annotation names the job alone as it did before.
step=""
for _ in 1 2 3; do
  step="$(gh api "repos/${repo}/actions/runs/${run}/jobs?per_page=100" \
    --jq "([.jobs[] | select(.runner_name == \"${runner}\" and .status == \"in_progress\")][0].steps // []) | map(select(.conclusion == \"failure\")) | .[0].name // empty" \
    2>/dev/null || true)"
  if [ -n "$step" ]; then
    break
  fi
  sleep 2
done

if [ -n "$step" ]; then
  echo "::error title=First red: ${job} / ${step}::${job} failed at step \"${step}\"; cancelling the rest of the run"
else
  echo "::error title=First red::${job} failed; cancelling the rest of the run"
fi
gh api --method POST "repos/${repo}/actions/runs/${run}/cancel" || true
