//! `state/needs-you.json` — the Drive's "Needs you" list, written when a
//! Captain's turn ends.
//!
//! The list is parsed out of the Drive's `DIGEST.md` (`## Needs you` bullets).
//! The file is rewritten only when the items change, so a poller can watch its
//! mtime; a turn that fails or is interrupted writes nothing and the last good
//! list survives, with the outcome saying so.

use chrono::{DateTime, Utc};
use std::path::Path;
use zeron_proto::DoneStatus;

/// The file's name inside `<drive>/state/`.
pub const FILE: &str = "needs-you.json";

/// The only shape this file has ever had, and the only one it will.
pub const VERSION: u32 = 1;

/// A digest that lists more than this many asks is a digest nobody reads.
const CAP: usize = 50;

/// What a turn end did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The items changed and the file was rewritten.
    Wrote,
    /// The items are identical: nothing was written and `updatedAt` is the
    /// previous one.
    Unchanged,
    /// The turn did not complete: the last good list was kept and nothing was
    /// written.
    KeptLastGood,
}

/// What one call did, for the caller to trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub outcome: Outcome,
    /// How many items the list holds after the call.
    pub items: usize,
}

/// The `## Needs you` bullets of `markdown`: `-`, `*` and `N.` list items,
/// trimmed, empties dropped, capped. Harmless on a file with no such section.
pub fn parse_needs_you(markdown: &str) -> Vec<String> {
    let _ = markdown;
    Vec::new()
}

/// Snapshot `drive`'s `DIGEST.md` into `<drive>/state/needs-you.json`.
///
/// A missing `DIGEST.md` is an empty list, not an error. Identical items rewrite
/// nothing and keep the previous `updatedAt`; a changed list lands atomically.
pub fn record_needs_you(drive: &Path, now: DateTime<Utc>) -> Result<Recorded, String> {
    let _ = (drive, now);
    Ok(Recorded {
        outcome: Outcome::Unchanged,
        items: 0,
    })
}

/// The turn-end rule: a completed turn snapshots the digest; an errored or
/// interrupted turn keeps the last good list and writes nothing.
pub fn record_turn_end(
    drive: &Path,
    status: DoneStatus,
    now: DateTime<Utc>,
) -> Result<Recorded, String> {
    // RED SCAFFOLD: the failed-turn branch is the implementation step.
    let _ = status;
    record_needs_you(drive, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_digest(drive: &Path, body: &str) {
        std::fs::create_dir_all(drive).expect("drive");
        std::fs::write(drive.join("DIGEST.md"), body).expect("digest");
    }

    fn record(drive: &Path) -> Recorded {
        record_needs_you(drive, Utc::now()).expect("record")
    }

    fn stored(drive: &Path) -> serde_json::Value {
        let raw = std::fs::read_to_string(drive.join("state").join(FILE)).expect("read the file");
        serde_json::from_str(&raw).expect("valid json")
    }

    #[test]
    fn the_needs_you_section_is_parsed_and_capped() {
        let markdown = "# Digest\n\n## New\n- not an ask\n\n## Needs you\n\
                        - approve the deploy\n* pick a name\n1. choose the repo\n\n\
                        ## In progress\n- ignored\n";
        assert_eq!(
            parse_needs_you(markdown),
            vec!["approve the deploy", "pick a name", "choose the repo"]
        );

        // No such section at all is an empty list, not an error.
        assert!(parse_needs_you("# Digest\n\n## New\n- x\n").is_empty());

        // The cap keeps a runaway digest from becoming a runaway file.
        let many = format!(
            "## Needs you\n{}",
            (0..60).map(|i| format!("- item {i}\n")).collect::<String>()
        );
        let parsed = parse_needs_you(&many);
        assert_eq!(parsed.len(), 50);
        assert_eq!(parsed[0], "item 0");
        assert_eq!(parsed[49], "item 49");
    }

    #[test]
    fn the_file_has_the_documented_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_digest(dir.path(), "## Needs you\n- one ask\n");
        assert_eq!(
            record(dir.path()),
            Recorded {
                outcome: Outcome::Wrote,
                items: 1
            }
        );

        let stored = stored(dir.path());
        assert_eq!(stored["version"], 1);
        assert_eq!(stored["items"], serde_json::json!(["one ask"]));
        let stamp = stored["updatedAt"].as_str().expect("updatedAt is a string");
        assert!(
            chrono::DateTime::parse_from_rfc3339(stamp).is_ok(),
            "updatedAt is RFC 3339: {stamp}"
        );
        assert_eq!(
            stored.as_object().expect("object").len(),
            3,
            "exactly version, updatedAt and items: {stored}"
        );
    }

    #[test]
    fn an_unchanged_list_is_not_rewritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_digest(dir.path(), "## Needs you\n- one ask\n- another ask\n");
        assert_eq!(record(dir.path()).outcome, Outcome::Wrote);
        let path = dir.path().join("state").join(FILE);
        let first = std::fs::read_to_string(&path).expect("first");
        let first_mtime = std::fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        // Filesystems with coarse mtimes must not make this assertion flaky.
        std::thread::sleep(std::time::Duration::from_millis(20));

        // The same asks, with cosmetic churn: the items did not change.
        write_digest(
            dir.path(),
            "## Needs you\n\n- one ask\n\n- another ask\n\n## New\n- later\n",
        );
        assert_eq!(
            record(dir.path()),
            Recorded {
                outcome: Outcome::Unchanged,
                items: 2
            }
        );
        assert_eq!(std::fs::read_to_string(&path).expect("second"), first);
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .modified()
                .expect("mtime"),
            first_mtime,
            "an unchanged list does not touch the file"
        );
    }

    #[test]
    fn a_new_ask_rewrites_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_digest(dir.path(), "## Needs you\n- one ask\n");
        assert_eq!(record(dir.path()).outcome, Outcome::Wrote);
        write_digest(dir.path(), "## Needs you\n- one ask\n- a second ask\n");
        assert_eq!(
            record(dir.path()),
            Recorded {
                outcome: Outcome::Wrote,
                items: 2
            }
        );
        assert_eq!(
            stored(dir.path())["items"],
            serde_json::json!(["one ask", "a second ask"])
        );
    }

    #[test]
    fn a_missing_digest_is_an_empty_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The Drive exists but has never had a DIGEST.md written.
        std::fs::create_dir_all(dir.path()).expect("drive");
        assert_eq!(
            record(dir.path()),
            Recorded {
                outcome: Outcome::Wrote,
                items: 0
            }
        );
        assert_eq!(stored(dir.path())["items"], serde_json::json!([]));
    }

    #[test]
    fn a_digest_without_the_section_empties_the_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_digest(dir.path(), "## Needs you\n- one ask\n");
        assert_eq!(record(dir.path()).outcome, Outcome::Wrote);
        // The Captain cleared its asks; the file must say so, once.
        write_digest(dir.path(), "## New\n- nothing waiting\n");
        assert_eq!(
            record(dir.path()),
            Recorded {
                outcome: Outcome::Wrote,
                items: 0
            }
        );
        assert_eq!(stored(dir.path())["items"], serde_json::json!([]));
    }

    /// The first red case: a failed turn must not clobber the list, and the
    /// returned outcome must say the last good list was kept.
    #[test]
    fn an_errored_turn_keeps_the_last_good_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_digest(dir.path(), "## Needs you\n- keep me\n");
        assert_eq!(record(dir.path()).outcome, Outcome::Wrote);
        let path = dir.path().join("state").join(FILE);
        let before = std::fs::read_to_string(&path).expect("before");
        let before_stamp = stored(dir.path())["updatedAt"]
            .as_str()
            .expect("updatedAt")
            .to_owned();
        let before_mtime = std::fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");

        // The sidecar died mid-turn after the digest changed: the new ask must
        // not overwrite the list the person last saw.
        write_digest(dir.path(), "## Needs you\n- half-written ask\n");
        let now = Utc::now();
        assert_eq!(
            record_turn_end(dir.path(), DoneStatus::Errored, now).expect("record"),
            Recorded {
                outcome: Outcome::KeptLastGood,
                items: 1
            }
        );
        assert_eq!(std::fs::read_to_string(&path).expect("after"), before);
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .modified()
                .expect("mtime"),
            before_mtime,
            "a failed turn does not touch the file"
        );
        assert_eq!(
            stored(dir.path())["updatedAt"],
            serde_json::json!(before_stamp),
            "the previous updatedAt survives"
        );
    }

    /// The second red case: an interrupted (sidecar-death) turn is the same
    /// rule as an errored one — keep the last good list.
    #[test]
    fn an_interrupted_turn_keeps_the_last_good_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_digest(dir.path(), "## Needs you\n- keep me\n");
        assert_eq!(record(dir.path()).outcome, Outcome::Wrote);
        let path = dir.path().join("state").join(FILE);
        let before = std::fs::read_to_string(&path).expect("before");

        write_digest(dir.path(), "## Needs you\n- interrupted ask\n");
        assert_eq!(
            record_turn_end(dir.path(), DoneStatus::Interrupted, Utc::now()).expect("record"),
            Recorded {
                outcome: Outcome::KeptLastGood,
                items: 1
            }
        );
        assert_eq!(std::fs::read_to_string(&path).expect("after"), before);
    }

    /// A completed turn still snapshots, so the failed-turn branch is a branch
    /// and not a second implementation.
    #[test]
    fn a_completed_turn_records_the_new_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_digest(dir.path(), "## Needs you\n- one ask\n");
        assert_eq!(
            record_turn_end(dir.path(), DoneStatus::Completed, Utc::now()).expect("record"),
            Recorded {
                outcome: Outcome::Wrote,
                items: 1
            }
        );
        assert_eq!(
            stored(dir.path())["items"],
            serde_json::json!(["one ask"])
        );
    }
}
