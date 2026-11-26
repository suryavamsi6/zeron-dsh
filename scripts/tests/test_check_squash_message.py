"""scripts/ci/check_squash_message.py: the squash message must carry the
exact verdict line implied by the PR's newest SHIP review."""

import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "ci" / "check_squash_message.py"
SHA = "3a894ab736588fcd773d90a28f04874508c14fef"
LINE = f"verdict 6040806665 (SHIP sur254-r1-e9a1871e) pinned to {SHA}"

FAKE_GH = """#!/usr/bin/env bash
printf '%s\n' "${FAKE_REVIEWS:-[]}"
"""


def review(body, review_id=6040806665, sha=SHA, submitted="2026-11-25T00:00:00Z"):
    return {"id": review_id, "commit_id": sha, "state": "COMMENTED", "body": body,
            "submitted_at": submitted}


class SquashMessageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="squash-message-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        tmp = pathlib.Path(self.tmp)
        (tmp / "bin").mkdir()
        gh = tmp / "bin" / "gh"
        gh.write_text(FAKE_GH)
        gh.chmod(0o755)
        self.message = tmp / "message"
        self.env = {
            "PATH": f"{tmp / 'bin'}{os.pathsep}{os.environ['PATH']}",
            "FAKE_REVIEWS": json.dumps([
                review("VERDICT: SHIP sur254-r1-e9a1871e\n\nLooks good."),
            ]),
        }

    def run_check(self, text):
        self.message.write_text(text)
        return subprocess.run(
            ["python3", str(SCRIPT), "o/r", "168", str(self.message)],
            env=self.env,
            capture_output=True,
            text=True,
        )

    def test_a_correct_squash_message_passes(self):
        result = self.run_check(f"feat: land the crew change\n\n{LINE}\n")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_missing_verdict_line_is_refused(self):
        result = self.run_check("feat: land the crew change\n\nCo-authored-by: Builder\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing required line", result.stderr)
        self.assertIn(LINE, result.stderr)

    def test_a_different_nonce_or_sha_is_not_the_reviewed_verdict(self):
        for line in [
            LINE.replace("sur254-r1-e9a1871e", "sur254-r1-other"),
            LINE.replace(SHA, "0" * 40),
        ]:
            result = self.run_check(f"subject\n\n{line}\n")
            self.assertEqual(result.returncode, 1, line)

    def test_the_newest_ship_review_is_the_one_required(self):
        newer_sha = "1" * 40
        self.env["FAKE_REVIEWS"] = json.dumps([
            review("VERDICT: SHIP old-nonce", review_id=1, sha="0" * 40),
            review("VERDICT: SHIP new-nonce", review_id=2, sha=newer_sha),
        ])
        required = f"verdict 2 (SHIP new-nonce) pinned to {newer_sha}"
        result = self.run_check(f"subject\n\n{required}\n")
        self.assertEqual(result.returncode, 0, result.stderr)


    def test_a_quoted_ship_line_is_not_a_new_verdict(self):
        self.env["FAKE_REVIEWS"] = json.dumps([
            review("VERDICT: SHIP real-nonce", review_id=1, sha="0" * 40,
                   submitted="2026-11-25T00:00:00Z"),
            review("```\nVERDICT: SHIP real-nonce\n```\nquoted in a reply", review_id=2,
                   sha="1" * 40, submitted="2026-11-25T01:00:00Z"),
        ])
        required = f"verdict 1 (SHIP real-nonce) pinned to {'0' * 40}"
        result = self.run_check(f"subject\n\n{required}\n")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_submission_order_beats_draft_review_id_order(self):
        self.env["FAKE_REVIEWS"] = json.dumps([
            review("VERDICT: SHIP submitted-last", review_id=1, sha="0" * 40,
                   submitted="2026-11-25T02:00:00Z"),
            review("VERDICT: SHIP submitted-first", review_id=2, sha="1" * 40,
                   submitted="2026-11-25T01:00:00Z"),
        ])
        required = f"verdict 1 (SHIP submitted-last) pinned to {'0' * 40}"
        result = self.run_check(f"subject\n\n{required}\n")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_paginated_review_pages_are_flattened(self):
        self.env["FAKE_REVIEWS"] = json.dumps([[review("no verdict", review_id=1)],
                                               [review("VERDICT: SHIP page-two", review_id=2,
                                                       sha="1" * 40)]])
        required = f"verdict 2 (SHIP page-two) pinned to {'1' * 40}"
        result = self.run_check(f"subject\n\n{required}\n")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_pr_without_a_ship_review_is_refused(self):
        self.env["FAKE_REVIEWS"] = json.dumps([review("no verdict here")])
        result = self.run_check(f"subject\n\n{LINE}\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("no review carrying", result.stderr)


if __name__ == "__main__":
    unittest.main()
