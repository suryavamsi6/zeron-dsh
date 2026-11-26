#!/usr/bin/env python3
"""Refuse a squash message that drops the reviewed verdict line.

The Captain review convention ends in a GitHub review whose body starts with
`VERDICT: SHIP <nonce>`. The permanent squash message must carry the same
review as `verdict <review-id> (SHIP <nonce>) pinned to <review-sha>`, where
the id and SHA come from that review object rather than from hand-copied text.

Usage: check_squash_message.py <owner/repo> <pr-number> <message-file>
"""

import json
import re
import subprocess
import sys

VERDICT = re.compile(r"^VERDICT:\s+SHIP\s+(?P<nonce>\S+)\s*$")


def verdict_line(reviews):
    """The canonical line required from the newest review carrying a verdict."""
    ships = []
    for review in reviews:
        body = review.get("body") or ""
        first = next((line.rstrip() for line in body.splitlines() if line.strip()), "")
        match = VERDICT.fullmatch(first)
        if not match:
            continue
        try:
            review_id = int(review["id"])
        except (KeyError, TypeError, ValueError):
            continue
        sha = review.get("commit_id") or ""
        if not re.fullmatch(r"[0-9a-fA-F]{40}", sha):
            continue
        # Review ids are minted when a draft is created; submission order is
        # the approval order. ISO timestamps sort chronologically.
        ships.append((review.get("submitted_at") or "", review_id, match.group("nonce"), sha.lower()))
    if not ships:
        return None
    _, review_id, nonce, sha = max(ships)
    return f"verdict {review_id} (SHIP {nonce}) pinned to {sha}"


def missing_line(message, line):
    """Whether the exact canonical line is absent from the planned message."""
    return line not in message.splitlines()


def review_pages(payload):
    """`gh api --paginate --slurp` wraps each page in its own array."""
    if all(isinstance(page, list) for page in payload):
        return [review for page in payload for review in page]
    return payload


def gh_reviews(repo, pr):
    out = subprocess.run(
        [
            "gh",
            "api",
            f"repos/{repo}/pulls/{pr}/reviews?per_page=100",
            "--paginate",
            "--slurp",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return review_pages(json.loads(out.stdout))


def main(argv):
    if len(argv) != 4:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    repo, pr, message_path = argv[1:]
    try:
        reviews = gh_reviews(repo, pr)
    except (subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"squash message check: could not read PR reviews: {error}", file=sys.stderr)
        return 2
    line = verdict_line(reviews)
    if line is None:
        print(
            f"squash message check: PR #{pr} has no review carrying `VERDICT: SHIP <nonce>`",
            file=sys.stderr,
        )
        return 1
    try:
        with open(message_path, encoding="utf-8") as message_file:
            message = message_file.read()
    except OSError as error:
        print(f"squash message check: could not read {message_path}: {error}", file=sys.stderr)
        return 2
    if missing_line(message, line):
        print(f"squash message check: missing required line:\n{line}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
