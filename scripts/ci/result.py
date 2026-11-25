#!/usr/bin/env python3
"""Aggregate the CI suite results without confusing selection for a pass.

`needs.<suite>.result == "skipped"` is legitimate only when `plan` did not
select that suite. A selected suite that skips means its workflow condition is
broken; an empty or incomplete selection can never be a green gate.
"""

import json
import sys

CONTROL_JOBS = ("plan", "admit")
# needs job -> the key plan.py emits in its `suites` map.
SUITE_JOBS = {
    "linux-core": "core",
    "deepseek": "deepseek",
    "edge": "edge",
    "installer": "installer",
    "voice": "voice",
    "windows": "windows",
    "ios": "ios",
    "android": "android",
    "zgx": "zgx",
}


def evaluate(needs):
    errors = []
    suites = dict(SUITE_JOBS)

    for name in CONTROL_JOBS:
        result = needs.get(name, {}).get("result")
        if result != "success":
            errors.append(f"control job {name}={result!r}")

    try:
        selected = json.loads(needs.get("plan", {}).get("outputs", {}).get("suites", ""))
    except (TypeError, json.JSONDecodeError):
        selected = None
    if not isinstance(selected, dict) or not selected:
        errors.append("plan emitted an empty or malformed suites map")
        selected = {}

    outputs = needs.get("plan", {}).get("outputs", {})
    try:
        skipped_jobs = json.loads(outputs.get("skip", "{}"))
    except (TypeError, json.JSONDecodeError):
        skipped_jobs = {}
        errors.append("plan emitted a malformed skip map")
    if not isinstance(skipped_jobs, dict):
        skipped_jobs = {}
        errors.append("plan emitted a malformed skip map")

    missing = sorted(key for key in suites.values() if key not in selected)
    if missing:
        errors.append("plan suites map is missing: " + ", ".join(missing))

    chosen = sorted(
        key for key in suites.values() if selected.get(key) is True or skipped_jobs.get(key)
    )
    if not chosen:
        errors.append("plan selected zero suites")

    for name, key in sorted(suites.items()):
        result = needs.get(name, {}).get("result")
        print(name.ljust(13), result)
        if selected.get(key) is True:
            if result != "success":
                errors.append(f"selected suite {name}={result}")
        elif result not in ("success", "skipped"):
            errors.append(f"not success or skipped: {name}={result}")
    return errors


def main():
    needs = json.load(sys.stdin)
    errors = evaluate(needs)
    if errors:
        for error in errors:
            print(error)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
