//! `reports/<chat>.md` — a crew chat's turn report, written into the
//! CAPTAIN's Drive when a crew turn ends.
//!
//! The file is replaced every turn, so the report always reads as the latest
//! turn: the chat's id, the turn's outcome, when it ran, and its last
//! assistant text — the part a Captain reads, bounded so a runaway answer
//! cannot make a runaway file.

use chrono::{DateTime, Utc};
use std::io::Write;
use std::path::{Path, PathBuf};
use zeron_proto::DoneStatus;

/// The folder inside the Captain's Drive a crew report lands in.
pub const DIR: &str = "reports";

/// A report body is bounded at this many chars; past it the body is cut and
/// the file says so.
const CAP: usize = 4_000;

/// The note a capped body ends with.
const TRUNCATED: &str = "[truncated]";

/// The wake line's copy of the turn's last reply is bounded at this many
/// chars.
const REPLY_CAP: usize = 600;

/// The outcome word the report's Status line and the wake line share.
fn status_word(status: DoneStatus) -> &'static str {
    match status {
        DoneStatus::Completed => "completed",
        DoneStatus::Interrupted => "aborted",
        DoneStatus::Errored => "error",
        // Not the provider's wire literal: the report's own plain-English
        // outcome, so the finish reason is not copied across the seam.
        DoneStatus::MaxTokens => "token-capped",
    }
}

/// `<drive>/reports/<chat_id>.md`, or `None` when `chat_id` is not a bare
/// filename: empty, `.`, `..`, or anything containing `/`, `\` or NUL.
pub fn report_path(drive: &Path, chat_id: &str) -> Option<PathBuf> {
    if chat_id.is_empty()
        || chat_id == "."
        || chat_id == ".."
        || chat_id.contains(['/', '\\', '\0'])
    {
        return None;
    }
    Some(drive.join(DIR).join(format!("{chat_id}.md")))
}

/// Write (replace) the crew report at `drive`, atomically: temp file in
/// `reports/` then rename. `status` is the engine's `DoneStatus`.
pub fn write_report(
    drive: &Path,
    chat_id: &str,
    status: DoneStatus,
    text: &str,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let Some(path) = report_path(drive, chat_id) else {
        return Err(format!("{chat_id:?} is not a bare filename"));
    };
    let dir = drive.join(DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(&dir)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    let word = status_word(status);
    let mut body: String = text.chars().take(CAP).collect();
    if text.chars().count() > CAP {
        body.push_str(TRUNCATED);
    }
    let file = format!(
        "# Crew report: {chat_id}\n- Status: {word}\n- At: {}\n\n{body}\n",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    temp.write_all(file.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    temp.as_file()
        .sync_all()
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    temp.persist(&path)
        .map_err(|error| format!("cannot write {}: {}", path.display(), error.error))?;
    Ok(())
}

/// The line a crew turn's end queues to the chat's Captain (SUR-87): the
/// header — the crew chat's title ("a crew chat" when untitled), its short
/// id, the turn's outcome word — then the turn's last reply bounded at 600
/// chars, then where the full report is. `written` is whether the report
/// file landed: when it did not, the pointer says the report is pending
/// rather than naming a file that is not there.
pub fn report_text(
    title: &str,
    chat_id: &str,
    status: DoneStatus,
    text: &str,
    written: bool,
) -> String {
    let title = if title.is_empty() { "a crew chat" } else { title };
    let short: String = chat_id.chars().take(8).collect();
    let reply: String = text.chars().take(REPLY_CAP).collect();
    let pointer = if written {
        format!("Full report: {DIR}/{chat_id}.md")
    } else {
        format!("Full report: pending — {DIR}/{chat_id}.md is unavailable")
    };
    format!(
        "[crew report] {title} ({short}): {}\n{reply}\n{pointer}",
        status_word(status)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_path_takes_only_a_bare_filename() {
        let drive = Path::new("/drive");
        assert_eq!(
            report_path(drive, "chat-1"),
            Some(drive.join(DIR).join("chat-1.md"))
        );
        for bad in ["", ".", "..", "a/b", "a\\b"] {
            assert_eq!(report_path(drive, bad), None, "{bad:?} is not bare");
        }
    }

    #[test]
    fn a_report_body_is_capped_at_four_thousand_chars_with_a_note() {
        let dir = tempfile::tempdir().expect("tempdir");
        let text = "x".repeat(4_001);
        write_report(dir.path(), "chat-1", DoneStatus::Completed, &text, Utc::now())
            .expect("write");
        let file = std::fs::read_to_string(dir.path().join(DIR).join("chat-1.md")).expect("read");
        let body = file.split_once("\n\n").expect("header then body").1;
        assert!(body.contains(TRUNCATED), "a capped body says so: {body:.40}");
        assert_eq!(
            body.chars().count(),
            CAP + TRUNCATED.len() + 1,
            "4,000 chars plus the note and the closing newline"
        );
    }

    #[test]
    fn a_body_at_the_cap_is_not_marked_truncated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let text = "x".repeat(4_000);
        write_report(dir.path(), "chat-1", DoneStatus::Completed, &text, Utc::now())
            .expect("write");
        let file = std::fs::read_to_string(dir.path().join(DIR).join("chat-1.md")).expect("read");
        assert!(
            !file.contains(TRUNCATED),
            "a body exactly at the cap is whole: {:.40}",
            file
        );
    }
}
