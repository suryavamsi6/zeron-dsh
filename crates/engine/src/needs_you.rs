//! `state/needs-you.json` — the Drive's "Needs you" list, written when a
//! Captain's turn ends.
//!
//! The list is parsed out of the Drive's `DIGEST.md` (`## Needs you` bullets).
//! The file is rewritten only when the items change, so a poller can watch its
//! mtime; a turn that fails or is interrupted writes nothing and the last good
//! list survives, with the outcome saying so.

use chrono::{DateTime, Utc};
use std::io::Write;
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

/// The lists last read per Captain, kept for the life of the process. Diffing
/// a fresh read against it is what makes an item "new": the first read of a
/// Captain only seeds, and a read that fails keeps the last good list.
#[derive(Default)]
pub struct Seen(std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>);

impl Seen {
    /// The items in `items` that the previous read for `captain` did not carry.
    /// `None` (a read or parse failure) keeps the previous list and returns
    /// nothing; a first read only seeds and returns nothing; a valid list
    /// replaces the baseline, so a valid empty list clears it.
    pub fn new_items(&self, captain: &str, items: Option<Vec<String>>) -> Vec<String> {
        let Some(items) = items else {
            return Vec::new(); // a failed read keeps the last good baseline
        };
        let mut seen = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = seen.insert(captain.to_owned(), items.clone());
        let Some(previous) = previous else {
            return Vec::new(); // the first read of a Captain only seeds
        };
        items
            .into_iter()
            .filter(|item| !previous.contains(item))
            .collect()
    }
}

/// The items `<drive>/state/needs-you.json` stores: `[]` when the file is
/// missing, unreadable, not version 1, or not a version-1 list.
pub fn read_needs_you(path: &Path) -> Vec<String> {
    read_needs_you_checked(path).unwrap_or_default()
}

/// The same read, `None` when the file exists but cannot be read or is not a
/// version-1 list — so a caller diffing items keeps its last good baseline.
pub(crate) fn read_needs_you_checked(path: &Path) -> Option<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Stored {
        version: u32,
        items: Vec<String>,
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(Vec::new()); // no file: a valid empty list
        }
        Err(_) => return None,
    };
    let stored: Stored = serde_json::from_str(&raw).ok()?;
    (stored.version == VERSION).then_some(stored.items)
}

/// The `## Needs you` bullets of `markdown`: `-`, `*` and `N.` list items,
/// trimmed, empties dropped, capped. Harmless on a file with no such section.
pub fn parse_needs_you(markdown: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_section = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if !in_section {
            in_section = trimmed.eq_ignore_ascii_case("## needs you");
            continue;
        }
        if trimmed.starts_with("## ") {
            break;
        }
        let item = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| numbered_item(trimmed))
            .map(str::trim)
            .filter(|item| !item.is_empty());
        if let Some(item) = item {
            items.push(item.to_string());
            if items.len() == CAP {
                break;
            }
        }
    }
    items
}

/// The rest of a `N. ` list marker (`"1. ask"` → `" ask"`), or `None` when
/// `line` is not one. The digits must run to the dot and a space must follow.
fn numbered_item(line: &str) -> Option<&str> {
    let (digits, rest) = line.split_once('.')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.strip_prefix(' ')
}

/// Snapshot `drive`'s `DIGEST.md` into `<drive>/state/needs-you.json`.
///
/// A missing `DIGEST.md` is an empty list, not an error. Identical items rewrite
/// nothing and keep the previous `updatedAt`; a changed list lands atomically.
pub fn record_needs_you(drive: &Path, now: DateTime<Utc>) -> Result<Recorded, String> {
    let digest_path = drive.join("DIGEST.md");
    let digest = match std::fs::read_to_string(&digest_path) {
        Ok(digest) => digest,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("cannot read {}: {error}", digest_path.display())),
    };
    let items = parse_needs_you(&digest);

    let state = drive.join("state");
    let path = state.join(FILE);
    // A stored list identical to the parsed one keeps its file — `updatedAt`
    // and mtime both stay the last write's. A corrupt file is not a stored
    // list at all: it is rewritten below.
    if let Ok(Some(stored)) = stored_items(&path)
        && stored == items
    {
        return Ok(Recorded {
            outcome: Outcome::Unchanged,
            items: items.len(),
        });
    }

    std::fs::create_dir_all(&state)
        .map_err(|error| format!("cannot create {}: {error}", state.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(&state)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    let body = format!(
        "{{\"version\":{VERSION},\"updatedAt\":\"{}\",\"items\":{}}}",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        serde_json::to_string(&items).expect("a string list serialises"),
    );
    temp.write_all(body.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    temp.as_file()
        .sync_all()
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    temp.persist(&path)
        .map_err(|error| format!("cannot write {}: {}", path.display(), error.error))?;
    Ok(Recorded {
        outcome: Outcome::Wrote,
        items: items.len(),
    })
}

/// The items `path` stores: `Ok(None)` when the file is absent, `Err` on a
/// read or parse failure.
fn stored_items(path: &Path) -> Result<Option<Vec<String>>, String> {
    #[derive(serde::Deserialize)]
    struct Stored {
        items: Vec<String>,
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    serde_json::from_str::<Stored>(&raw)
        .map(|stored| Some(stored.items))
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))
}

/// The turn-end rule: a completed turn snapshots the digest; an errored or
/// interrupted turn keeps the last good list and writes nothing.
pub fn record_turn_end(
    drive: &Path,
    status: DoneStatus,
    now: DateTime<Utc>,
) -> Result<Recorded, String> {
    match status {
        DoneStatus::Completed => record_needs_you(drive, now),
        // The digest mid-turn is not a good list: nothing is written and the
        // file already there — the last good list — is reported, not touched.
        DoneStatus::Errored | DoneStatus::Interrupted => Ok(Recorded {
            outcome: Outcome::KeptLastGood,
            items: stored_items(&drive.join("state").join(FILE))?
                .map_or(0, |items| items.len()),
        }),
    }
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

    #[test]
    fn captains_needs_you_a_first_read_seeds_without_notifying() {
        let seen = Seen::default();
        let items = Some(vec!["approve the deploy".to_string()]);
        assert!(seen.new_items("cap-a", items).is_empty());
    }

    #[test]
    fn captains_needs_you_new_items_notify_once_each() {
        let seen = Seen::default();
        seen.new_items("cap-a", Some(vec!["approve the deploy".to_string()]));
        let new = seen.new_items(
            "cap-a",
            Some(vec!["approve the deploy".to_string(), "pick a name".to_string()]),
        );
        assert_eq!(new, vec!["pick a name".to_string()]);
        // Read again: the same item is not new twice.
        assert!(seen
            .new_items(
                "cap-a",
                Some(vec!["approve the deploy".to_string(), "pick a name".to_string()]),
            )
            .is_empty());
    }

    #[test]
    fn captains_needs_you_unchanged_and_removed_do_not_notify() {
        let seen = Seen::default();
        seen.new_items("cap-a", Some(vec!["a".to_string(), "b".to_string()]));
        assert!(seen.new_items("cap-a", Some(vec!["a".to_string(), "b".to_string()])).is_empty());
        assert!(seen.new_items("cap-a", Some(vec!["a".to_string()])).is_empty());
    }

    #[test]
    fn captains_needs_you_a_failed_read_keeps_the_baseline_and_a_recovery_reports_only_new_items() {
        let seen = Seen::default();
        // Seeded: "a" has been seen.
        assert!(seen.new_items("cap-a", Some(vec!["a".to_string()])).is_empty());
        // The file cannot be read: nothing rings and the baseline stays.
        assert!(seen.new_items("cap-a", None).is_empty());
        // The recovery carries the seen item plus one new one: exactly the new
        // one rings. An implementation that dropped the baseline on the failed
        // read would treat this as a first read and ring nothing.
        assert_eq!(
            seen.new_items("cap-a", Some(vec!["a".to_string(), "b".to_string()])),
            vec!["b".to_string()]
        );
    }

    #[test]
    fn captains_needs_you_a_valid_empty_list_clears_the_baseline() {
        let seen = Seen::default();
        seen.new_items("cap-a", Some(vec!["a".to_string()]));
        assert!(seen.new_items("cap-a", Some(Vec::new())).is_empty());
        // Cleared, so the item returning is new again.
        assert_eq!(
            seen.new_items("cap-a", Some(vec!["a".to_string()])),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn captains_needs_you_reads_a_version_1_file_and_refuses_anything_else() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join(FILE);
        // Missing file: an empty list, not a failure.
        assert!(read_needs_you(&path).is_empty());
        // A version-1 list reads.
        std::fs::write(
            &path,
            r#"{"version":1,"updatedAt":"2026-11-01T00:00:00Z","items":["approve the deploy"]}"#,
        )
        .expect("write");
        assert_eq!(read_needs_you(&path), vec!["approve the deploy".to_string()]);
        assert_eq!(
            read_needs_you_checked(&path),
            Some(vec!["approve the deploy".to_string()])
        );
        // Bad JSON is an empty list, but not a good snapshot.
        std::fs::write(&path, "{not json").expect("write");
        assert!(read_needs_you(&path).is_empty());
        assert_eq!(read_needs_you_checked(&path), None);
        // Another version is ignored.
        std::fs::write(&path, r#"{"version":2,"items":["other"]}"#).expect("write");
        assert!(read_needs_you(&path).is_empty());
        assert_eq!(read_needs_you_checked(&path), None);
    }
}
