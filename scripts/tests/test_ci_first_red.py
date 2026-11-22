"""scripts/ci/first-red.sh: publishes the failed step's excerpt, cancels only
a superseded pull-request head. A fake `gh` on PATH serves canned API JSON
(and records every call) — no network."""

import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "ci" / "first-red.sh"

# Records every call in $FAKE_GH_CALLS, then serves the canned body for the
# endpoint — $FAKE_JOBS for the run's jobs, $FAKE_PULLS for the pull request
# (failing when FAKE_PULLS_FAIL=1) — through the --jq filter like gh does.
FAKE_GH = """#!/usr/bin/env bash
echo "$*" >> "${FAKE_GH_CALLS:?}"
jq_expr=""; endpoint=""; prev=""
for a in "$@"; do
  if [ "$prev" = "--jq" ]; then jq_expr="$a"; prev=""; continue; fi
  case "$prev" in --method|-X) prev=""; continue;; esac
  case "$a" in
    api) ;;
    --jq) prev="--jq" ;;
    --method|-X) prev="--method" ;;
    -*) ;;
    *) [ -z "$endpoint" ] && endpoint="$a" ;;
  esac
done
case "$endpoint" in
  *"/jobs"*) body="${FAKE_JOBS:-{\\"jobs\\": []}}" ;;
  *pulls/*) [ "${FAKE_PULLS_FAIL:-0}" = 1 ] && exit 1; body="$FAKE_PULLS" ;;
  */cancel) exit 0 ;;
  *) exit 1 ;;
esac
if [ -n "$jq_expr" ]; then printf '%s' "$body" | jq -r "$jq_expr"; else printf '%s\\n' "$body"; fi
"""

EVENT_SHA = "aaa111"


def jobs(*steps):
    """One in_progress job on runner-1; steps are (name, conclusion) pairs."""
    return {"jobs": [{"runner_name": "runner-1", "status": "in_progress",
                      "steps": [{"name": n, "conclusion": c} for n, c in steps]}]}


RED_JOBS = jobs(("Checkout", "success"), ("Build tests", "failure"),
                ("First red excerpt; cancel a superseded run", None))
GREEN_JOBS = jobs(("Checkout", "success"), ("Build tests", "success"),
                  ("First red excerpt; cancel a superseded run", None))
# An earlier step still shown running (the API lags its conclusion, or the
# script ran by hand mid-job): not provably green, so the job-only
# annotation publishes after the retries.
PENDING_JOBS = jobs(("Checkout", "success"), ("Build tests", None))

# The failed step's section, then a later step that kept running and passed:
# the excerpt must come from Build tests, not from the file's tail.
LOG = """\
first-red-step: Checkout
Cloning into 'zeron-dsh'...
first-red-step: Build tests
  Compiling zeron-engine v0.1.0
   Compiling zeron-harness v0.1.0
error[E0063]: missing field `mode` in initializer of `plan::Plan`
  --> crates/engine/src/plan.rs:42:10
   |
42 |     Plan { id: 7 }
   |     ^^^^ missing `mode`
error: could not compile `zeron-engine` (lib) due to 1 previous error
warning: build failed, waiting for other jobs to finish...
first-red-step: Credential vault
added 40 packages, and audited 41 packages in 3s
found 0 vulnerabilities
"""


class FirstRedTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="first-red-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        tmp = pathlib.Path(self.tmp)
        (tmp / "bin").mkdir()
        gh = tmp / "bin" / "gh"
        gh.write_text(FAKE_GH)
        gh.chmod(0o755)
        (tmp / "rt").mkdir()  # $RUNNER_TEMP
        self.calls = tmp / "calls"
        self.summary = tmp / "summary"
        event = tmp / "event.json"
        event.write_text(json.dumps({"pull_request": {"number": 7, "head": {"sha": EVENT_SHA}}}))
        self.env = {
            "PATH": f"{tmp / 'bin'}{os.pathsep}{os.environ['PATH']}",
            "FAKE_GH_CALLS": str(self.calls),
            "GITHUB_REPOSITORY": "o/r",
            "GITHUB_RUN_ID": "99",
            "GITHUB_JOB": "tests",
            "RUNNER_NAME": "runner-1",
            "RUNNER_TEMP": str(tmp / "rt"),
            "GITHUB_EVENT_PATH": str(event),
            "GITHUB_STEP_SUMMARY": str(self.summary),
            "FIRST_RED_RETRY_DELAY": "0",
            "GH_TOKEN": "unused",
        }

    def run_script(self, jobs_doc=RED_JOBS, pulls_head=EVENT_SHA, pulls_fail=False,
                   log=LOG, event_extra=None):
        env = dict(self.env)
        env["FAKE_JOBS"] = json.dumps(jobs_doc)
        env["FAKE_PULLS"] = json.dumps({"head": {"sha": pulls_head}})
        env["FAKE_PULLS_FAIL"] = "1" if pulls_fail else "0"
        if log is not None:
            (pathlib.Path(self.tmp) / "rt" / "first-red.log").write_text(log)
        if event_extra is not None:
            pathlib.Path(self.env["GITHUB_EVENT_PATH"]).write_text(json.dumps(event_extra))
        proc = subprocess.run(["bash", str(SCRIPT)], env={**os.environ, **env},
                              capture_output=True, text=True, timeout=30)
        calls = self.calls.read_text() if self.calls.exists() else ""
        summary = self.summary.read_text() if self.summary.exists() else ""
        return proc, calls, summary

    def test_red_on_a_live_head_publishes_the_excerpt_and_does_not_cancel(self):
        proc, calls, summary = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("::error title=", proc.stdout)
        self.assertIn("Build tests", proc.stdout)
        self.assertIn("error[E0063]", proc.stdout)
        self.assertIn("missing field", proc.stdout)
        self.assertIn("```", summary)  # fenced excerpt in the summary
        self.assertIn("error[E0063]", summary)
        self.assertNotIn("vulnerabilities", summary)  # not the later step's tail
        self.assertNotIn("/cancel", calls)

    def test_red_on_a_superseded_head_publishes_then_cancels(self):
        proc, calls, summary = self.run_script(pulls_head="bbb222")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("::error title=", proc.stdout)
        self.assertIn("error[E0063]", proc.stdout)
        self.assertIn("error[E0063]", summary)
        # publish first: the annotation precedes the cancel in the output
        self.assertLess(proc.stdout.index("::error"), proc.stdout.index("cancel"))
        self.assertIn("/cancel", calls)

    def test_pulls_api_failure_publishes_but_does_not_cancel(self):
        proc, calls, summary = self.run_script(pulls_fail=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("::error title=", proc.stdout)
        self.assertIn("error[E0063]", summary)
        self.assertIn("pulls/7", calls)
        self.assertNotIn("/cancel", calls)

    def test_nothing_failed_publishes_nothing(self):
        proc, calls, summary = self.run_script(jobs_doc=GREEN_JOBS)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertNotIn("::error", proc.stdout)
        self.assertNotIn("::notice", proc.stdout)
        self.assertEqual(proc.stdout.strip(), "")
        self.assertEqual(summary, "")
        self.assertNotIn("/cancel", calls)
        self.assertNotIn("pulls/", calls)  # nothing to decide: never asked

    def test_no_log_file_falls_back_to_the_step_name(self):
        proc, calls, summary = self.run_script(log=None)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('::error title=First red%3A tests / Build tests::tests failed at step "Build tests"',
                      proc.stdout)
        self.assertIn("`Build tests`", summary)
        self.assertNotIn("/cancel", calls)

    def test_an_unsettled_step_publishes_the_job_only_annotation(self):
        proc, calls, summary = self.run_script(jobs_doc=PENDING_JOBS)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('::error title=First red::tests failed', proc.stdout)
        self.assertNotIn("/cancel", calls)

    def test_a_non_pull_request_payload_never_cancels(self):
        proc, calls, _ = self.run_script(pulls_head="bbb222", event_extra={"ref": "refs/heads/main"})
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("::error title=", proc.stdout)
        self.assertNotIn("/cancel", calls)


if __name__ == "__main__":
    unittest.main()
