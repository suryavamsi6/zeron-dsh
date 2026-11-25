"""scripts/ci/result.py: selected suites may not read `skipped` as a pass."""

import json
import pathlib
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts" / "ci"))

import result  # noqa: E402

PLAN_KEYS = tuple(result.SUITE_JOBS.values())
JOB_NAMES = tuple(result.SUITE_JOBS)


def all_off():
    return {name: False for name in PLAN_KEYS}


def needs(selected, results):
    return {
        "plan": {"result": "success", "outputs": {"suites": json.dumps(selected)}},
        "admit": {"result": "success", "outputs": {}},
        **{name: {"result": results.get(name, "skipped"), "outputs": {}} for name in JOB_NAMES},
    }


class ResultTests(unittest.TestCase):
    def test_a_green_payload_passes(self):
        selected = all_off() | {"core": True}
        payload = needs(selected, {"linux-core": "success"})
        self.assertEqual(result.evaluate(payload), [])

    def test_legitimately_unselected_skips_pass(self):
        selected = all_off() | {"core": True, "zgx": True}
        payload = needs(selected, {"linux-core": "success", "zgx": "success"})
        self.assertEqual(result.evaluate(payload), [])

    def test_a_selected_skip_fails_and_names_the_suite(self):
        errors = result.evaluate(needs(all_off() | {"zgx": True}, {"zgx": "skipped"}))
        self.assertIn("selected suite zgx=skipped", errors)

    def test_an_abandoned_selected_suite_still_fails(self):
        errors = result.evaluate(needs(all_off() | {"zgx": True}, {"zgx": "abandoned"}))
        self.assertIn("selected suite zgx=abandoned", errors)

    def test_an_empty_suites_map_never_passes(self):
        errors = result.evaluate(needs({}, {}))
        self.assertTrue(any("empty or malformed suites map" in error for error in errors), errors)
        self.assertIn("plan selected zero suites", errors)

    def test_a_missing_suite_key_never_passes(self):
        selected = {key: False for key in PLAN_KEYS if key != "zgx"} | {"core": True}
        errors = result.evaluate(needs(selected, {"linux-core": "success"}))
        self.assertIn("plan suites map is missing: zgx", errors)

    def test_zero_selected_suites_never_passes(self):
        self.assertIn("plan selected zero suites", result.evaluate(needs(all_off(), {})))

    def test_malformed_suites_json_never_passes(self):
        payload = needs(all_off(), {})
        payload["plan"]["outputs"]["suites"] = "{not json"
        errors = result.evaluate(payload)
        self.assertTrue(any("empty or malformed suites map" in error for error in errors), errors)

    def test_ci_uses_the_tested_script(self):
        ci = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
        self.assertIn("python3 scripts/ci/result.py", ci)


if __name__ == "__main__":
    unittest.main()
