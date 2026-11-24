//! The Captains pages' host actions: the device-local roster, the calls
//! that change it, and the reads of a Captain's Drive — the Digest tab's
//! summary, and the Drive tab's listing and a file's text.
//!
//! `ui.call` is a host action, not an engine RPC — asking it for `Captains`
//! answers "no host action named 'Captains'" — so each of these is one action
//! over one RPC method (`crates/rpc/src/lib.rs`: `Captains`, `PromoteCaptain`,
//! `DemoteCaptain`, `AddCrew`, `RemoveCrew`; SUR-94's cascade adds
//! `ArchiveCaptain`, `UnarchiveCaptain`, `DeleteCaptain` from
//! `crates/engine/src/captains.rs`), under the name the page calls.
//! The roster is the engine's (`crates/engine/src/captains.rs`), device-local:
//! nothing here reads or writes `captains.json`, and no action invents a
//! second route to it.
//!
//! `CaptainDrive` and `CaptainDriveRead` are not RPCs: the Drive is a folder
//! on this device (`crates/engine/src/captains.rs`'s `drive_dir`), and the
//! roster's `drivePath` is the only path they read.
//!
//! Field names are the engine's, unchanged: promoting takes `{chatId, name}`,
//! demoting and each cascade call take `{chatId}`, and the crew calls
//! `{captainId, chatId}`. An error the engine answers — a busy chat, a crew
//! member that cannot be promoted, a demoted Captain that cannot be hidden,
//! a refusal because the roster is another device's — reaches the page as the
//! host's error, with the engine's own words in `err.message`.

use serde_json::{Value as Json, json};
use zgx_script::{ScriptHost, Value};

use crate::auth::text;
use crate::engine;
use crate::providers::McpCall;

pub fn install(host: &ScriptHost) {
    install_over(host, |method, params| {
        let engine = engine::get();
        Box::pin(async move { engine.call(method, params).await })
    });
}

/// The roster calls over `call` — the same seam
/// [`crate::providers::install_mcp`] takes, so a test can drive them against
/// an engine on memory IPC instead of the process-wide one, and can record
/// which method each action dispatches.
pub fn install_over<F>(host: &ScriptHost, call: F)
where
    F: Fn(&'static str, Json) -> McpCall + Clone + 'static,
{
    // The roster as the engine holds it: `{}` -> `{ captains: [...] }`.
    host.async_action("Captains", {
        let call = call.clone();
        move |_| {
            let call = call.clone();
            async move { answer(call("Captains", json!({})).await) }
        }
    });
    // One chat promoted: `{ chatId, name }` -> the roster.
    host.async_action("PromoteCaptain", {
        let call = call.clone();
        move |args| {
            let call = call.clone();
            let chat_id = args.field("chatId").map(text).unwrap_or_default();
            let name = args.field("name").map(text).unwrap_or_default();
            async move {
                answer(call("PromoteCaptain", json!({ "chatId": chat_id, "name": name })).await)
            }
        }
    });
    // One chat demoted: `{ chatId }` -> the roster.
    host.async_action("DemoteCaptain", {
        let call = call.clone();
        move |args| {
            let call = call.clone();
            let chat_id = args.field("chatId").map(text).unwrap_or_default();
            async move { answer(call("DemoteCaptain", json!({ "chatId": chat_id })).await) }
        }
    });
    // One chat added to a crew: `{ captainId, chatId }` -> the roster.
    host.async_action("AddCrew", {
        let call = call.clone();
        move |args| {
            let call = call.clone();
            let captain_id = args.field("captainId").map(text).unwrap_or_default();
            let chat_id = args.field("chatId").map(text).unwrap_or_default();
            async move {
                answer(call("AddCrew", json!({ "captainId": captain_id, "chatId": chat_id })).await)
            }
        }
    });
    // One chat released from a crew: `{ captainId, chatId }` -> the roster.
    host.async_action("RemoveCrew", {
        let call = call.clone();
        move |args| {
            let call = call.clone();
            let captain_id = args.field("captainId").map(text).unwrap_or_default();
            let chat_id = args.field("chatId").map(text).unwrap_or_default();
            async move {
                answer(call("RemoveCrew", json!({ "captainId": captain_id, "chatId": chat_id })).await)
            }
        }
    });
    // The archive cascade's three (SUR-94): `{ chatId }` -> the roster, the
    // same thin forwards — an engine refusal (a demoted Captain cannot be
    // hidden) reaches the page as the host's error.
    host.async_action("CaptainArchive", {
        let call = call.clone();
        move |args| {
            let call = call.clone();
            let chat_id = args.field("chatId").map(text).unwrap_or_default();
            async move { answer(call("ArchiveCaptain", json!({ "chatId": chat_id })).await) }
        }
    });
    host.async_action("CaptainUnarchive", {
        let call = call.clone();
        move |args| {
            let call = call.clone();
            let chat_id = args.field("chatId").map(text).unwrap_or_default();
            async move { answer(call("UnarchiveCaptain", json!({ "chatId": chat_id })).await) }
        }
    });
    host.async_action("CaptainDelete", {
        let call = call.clone();
        move |args| {
            let call = call.clone();
            let chat_id = args.field("chatId").map(text).unwrap_or_default();
            async move { answer(call("DeleteCaptain", json!({ "chatId": chat_id })).await) }
        }
    });
    // A Captain's Drive, for the Digest and Drive tabs: `{ chatId }` ->
    //   { digest = <DIGEST.md's text>?, needsYou = { version, updatedAt,
    //     items }?, reports = { { name, text } },
    //     files = { { name, kind, size?, mtime? } } }
    // The roster decides whose Drive it is: a chat that is not a Captain on
    // this device is refused, and none of the four is a path the page gave.
    host.async_action("CaptainDrive", {
        let call = call.clone();
        move |args| {
            let call = call.clone();
            let chat_id = args.field("chatId").map(text).unwrap_or_default();
            async move {
                let roster = call("Captains", json!({})).await?;
                let drive = captain_drive(&roster, &chat_id)?;
                let digest = read_inside(&drive, &drive.join(DIGEST_FILE));
                let needs_you = needs_you(&drive);
                let reports = reports(&drive);
                let answer = json!({
                    "digest": digest,
                    "needsYou": needs_you,
                    "reports": reports,
                    "files": drive_files(&drive),
                });
                Value::from_json(&serde_json::to_string(&answer).map_err(|error| error.to_string())?)
                    .ok_or_else(|| "the Drive's answer didn't parse".to_string())
            }
        }
    });
    // One file of a Captain's Drive, by its Drive-relative path:
    // `{ chatId, path }` -> `{ text, size }`. A path leaving the Drive is an
    // error.
    host.async_action("CaptainDriveRead", {
        let call = call.clone();
        move |args| {
            let call = call.clone();
            let chat_id = args.field("chatId").map(text).unwrap_or_default();
            let path = args.field("path").map(text).unwrap_or_default();
            async move {
                let roster = call("Captains", json!({})).await?;
                let drive = captain_drive(&roster, &chat_id)?;
                let text = drive_read(&drive, &path)?;
                Value::from_json(&serde_json::to_string(&text).map_err(|error| error.to_string())?)
                    .ok_or_else(|| "the file's text didn't parse".to_string())
            }
        }
    });
}

/// The Drive of the Captain `chat_id` in `roster`, or the page's error: a
/// chat that is not a Captain here has no Drive to read, and the roster's
/// path is taken only when it is one.
fn captain_drive(roster: &Json, chat_id: &str) -> Result<std::path::PathBuf, String> {
    let path = if chat_id.is_empty() {
        None
    } else {
        roster
            .get("captains")
            .and_then(Json::as_array)
            .and_then(|list| {
                list.iter()
                    .find(|captain| captain.get("chatId").and_then(Json::as_str) == Some(chat_id))
            })
            .and_then(|captain| captain.get("drivePath"))
            .and_then(Json::as_str)
    };
    let drive = std::path::PathBuf::from(path.ok_or("That chat is not a Captain on this device")?);
    if !drive.is_absolute() {
        return Err("The Captain's Drive isn't known".to_string());
    }
    Ok(drive)
}

/// `path`'s text when it is a file inside `drive`, cut at [`FILE_CAP`]; nil
/// when it is missing, unreadable, or — followed to where it really is — a
/// file outside the Drive (a link planted inside it is not a way out).
fn read_inside(drive: &std::path::Path, path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let real = std::fs::canonicalize(path).ok()?;
    let root = std::fs::canonicalize(drive).ok()?;
    if !real.starts_with(&root) || !real.is_file() {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(&real).ok()?.take(FILE_CAP).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The engine's Needs-you snapshot (SUR-100), when it is there and is the
/// shape it has ever had: `{ version: 1, updatedAt, items: [string] }`. A
/// missing or malformed file is nil — the Digest tab stands on the markdown
/// alone, as it does before the Captain's first turn.
fn needs_you(drive: &std::path::Path) -> Option<Json> {
    let text = read_inside(drive, &drive.join(NEEDS_YOU_FILE))?;
    let stored: Json = serde_json::from_str(&text).ok()?;
    if stored.get("version").and_then(Json::as_u64) != Some(1) {
        return None;
    }
    let items: Vec<&str> = stored.get("items")?.as_array()?.iter().filter_map(Json::as_str).collect();
    Some(json!({
        "version": 1,
        "updatedAt": stored.get("updatedAt").cloned().unwrap_or(Json::Null),
        "items": items,
    }))
}

/// The newest [`REPORT_COUNT`] `reports/*.md`, newest first (by mtime, its
/// name breaking a tie), each as it is on disk.
fn reports(drive: &std::path::Path) -> Vec<Json> {
    let Ok(entries) = std::fs::read_dir(drive.join(REPORTS_DIR)) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, String, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let modified = entry.metadata().and_then(|meta| meta.modified()).unwrap_or(std::time::UNIX_EPOCH);
        found.push((modified, entry.file_name().to_string_lossy().into_owned(), path));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    found.truncate(REPORT_COUNT);
    found
        .into_iter()
        .filter_map(|(_, name, path)| {
            let text = read_inside(drive, &path)?;
            Some(json!({ "name": name, "text": text }))
        })
        .collect()
}

/// The Drive's files, flat: every top-level entry but `state/` (the
/// engine's own) plus the files directly under `reports/` — not the
/// directories there, not `handoffs/`'s contents — each `name` the
/// Drive-relative path `CaptainDriveRead` accepts. A file carries `size`
/// and `mtime` (RFC 3339 text, seconds, UTC, `Z`), a directory neither,
/// and a link is never listed. Sorted by `name` in byte order; a missing
/// or unreadable Drive is an empty list.
fn drive_files(drive: &std::path::Path) -> Vec<Json> {
    let mut listed: Vec<Json> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(drive) {
        for entry in entries.flatten() {
            if entry.file_name() == "state" {
                continue;
            }
            if let Some(one) = drive_entry(&entry, entry.file_name().to_string_lossy().into_owned())
            {
                listed.push(one);
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(drive.join(REPORTS_DIR)) {
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if !kind.is_file() {
                continue;
            }
            let name = format!("{}/{}", REPORTS_DIR, entry.file_name().to_string_lossy());
            if let Some(one) = drive_entry(&entry, name) {
                listed.push(one);
            }
        }
    }
    listed.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    listed
}

/// One listing row for `entry` under `name`: `{ name, kind }`, a file
/// carrying `size` (bytes) and `mtime` (RFC 3339 text, seconds, UTC, `Z`),
/// a directory carrying neither. A link — `file_type()` is the entry's own
/// type — and anything neither file nor directory is nil.
fn drive_entry(entry: &std::fs::DirEntry, name: String) -> Option<Json> {
    let kind = entry.file_type().ok()?;
    if kind.is_symlink() {
        return None;
    }
    if kind.is_dir() {
        return Some(json!({ "name": name, "kind": "directory" }));
    }
    if !kind.is_file() {
        return None;
    }
    let meta = entry.metadata().ok()?;
    let mtime = chrono::DateTime::<chrono::Utc>::from(
        meta.modified().unwrap_or(std::time::UNIX_EPOCH),
    )
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Some(json!({ "name": name, "kind": "file", "size": meta.len(), "mtime": mtime }))
}

/// One file's text by its Drive-relative path, through [`read_inside`]. An
/// empty path, an absolute one, or any `..` component — and, followed to
/// where it really is, anything outside the Drive or not a regular file —
/// is the host's error, not a read.
fn drive_read(drive: &std::path::Path, path: &str) -> Result<Json, String> {
    let relative = std::path::Path::new(path);
    if path.is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return Err("The path leaves the Captain's Drive".to_string());
    }
    let text = read_inside(drive, &drive.join(relative))
        .ok_or_else(|| "The file isn't in the Captain's Drive".to_string())?;
    let size = text.len();
    Ok(json!({ "text": text, "size": size }))
}

/// The Captain's Drive as the Digest tab reads it: `DIGEST.md`, the engine's
/// Needs-you snapshot (`state/needs-you.json`, SUR-100) and the crew reports
/// (`reports/*.md`, SUR-101), newest first.
///
/// The page names a chat, never a path: the Drive's path is the roster's
/// (`drivePath`, above), so nothing the page sends can point a read out of
/// it, and a file inside it that links out is not read either.
const DIGEST_FILE: &str = "DIGEST.md";
const NEEDS_YOU_FILE: &str = "state/needs-you.json";
const REPORTS_DIR: &str = "reports";
/// What the tab shows: a handful of reports is a digest, a folder of them is
/// a filing cabinet.
const REPORT_COUNT: usize = 5;
/// A file the tab shows at most: the Drive's files are the Captain's, and a
/// long one is cut rather than carried whole through the script bridge.
const FILE_CAP: u64 = 256 * 1024;

/// The engine's answer as a script value, or its refusal as the host error.
/// A copy of `providers`' own, kept here because this module's slice of the
/// app is the one file SUR-83 was granted (`apps/zgx/src/captains.rs`) plus the
/// registration line, not `providers.rs`.
fn answer(reply: Result<Json, String>) -> Result<Value, String> {
    let json = reply?;
    let text = serde_json::to_string(&json).map_err(|error| error.to_string())?;
    Value::from_json(&text).ok_or_else(|| "the engine's answer didn't parse".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Drive tab's listing: every top-level entry plus the files directly
    /// under `reports/` — nothing of `state/` (the engine's own) and nothing
    /// inside `handoffs/` — one flat array sorted by `name` in byte order, a
    /// directory carrying no `size`/`mtime`, a file carrying both.
    #[test]
    fn a_captains_drive_lists_one_level_and_the_reports_files() {
        let dir = tempfile::tempdir().unwrap();
        let drive = dir.path().join("drive");
        std::fs::create_dir_all(drive.join("state")).unwrap();
        std::fs::create_dir_all(drive.join("reports")).unwrap();
        std::fs::create_dir_all(drive.join("handoffs")).unwrap();
        std::fs::write(drive.join("DIGEST.md"), "the digest").unwrap();
        std::fs::write(drive.join("CREW.md"), "the crew").unwrap();
        std::fs::write(drive.join("state/needs-you.json"), "{}").unwrap();
        std::fs::write(drive.join("reports/2026-11-23-a.md"), "a report").unwrap();
        std::fs::write(drive.join("handoffs/x.md"), "a handoff").unwrap();

        let listed = serde_json::to_value(drive_files(&drive)).unwrap();
        let files = listed.as_array().unwrap();
        let names: Vec<&str> = files.iter().map(|entry| entry["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["CREW.md", "DIGEST.md", "handoffs", "reports", "reports/2026-11-23-a.md"],
            "the flat listing, sorted by name in byte order"
        );
        assert!(
            !listed.to_string().contains("state"),
            "state/ is the engine's: it appears nowhere in the listing"
        );
        for index in [2, 3] {
            let entry = &files[index];
            assert_eq!(entry["kind"], "directory", "{}", entry["name"]);
            assert!(
                entry.get("size").is_none() && entry.get("mtime").is_none(),
                "a directory carries no size or mtime key: {}",
                entry["name"]
            );
        }
        for index in [0, 1, 4] {
            let entry = &files[index];
            assert_eq!(entry["kind"], "file", "{}", entry["name"]);
            assert!(entry["size"].is_number(), "a file's size, in bytes: {}", entry["name"]);
            let mtime = entry["mtime"].as_str().unwrap_or_else(|| {
                panic!(
                    "a file's mtime is RFC 3339 text, not {}: {}",
                    entry["mtime"], entry["name"]
                )
            });
            assert!(
                mtime.ends_with('Z') && chrono::DateTime::parse_from_rfc3339(mtime).is_ok(),
                "a file's mtime is RFC 3339 in UTC with a Z, as the engine writes it: {} ({})",
                entry["name"],
                mtime
            );
        }
    }

    /// `CaptainDriveRead`'s read: the file's text back exactly — a blank line
    /// and the trailing newline survive the trip.
    #[test]
    fn a_captains_drive_reads_a_file_byte_identically() {
        let dir = tempfile::tempdir().unwrap();
        let drive = dir.path().join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        let body = "# Crew\n\nThe manifest ends with a newline.\n";
        std::fs::write(drive.join("CREW.md"), body).unwrap();

        let read = drive_read(&drive, "CREW.md").expect("CREW.md is in the Drive");
        assert_eq!(
            read["text"].as_str(),
            Some(body),
            "the Drive-relative path reads the file byte for byte"
        );
        assert_eq!(read["size"].as_u64(), Some(body.len() as u64), "the size is the bytes read");
    }

    /// Every way out of the Drive is refused as an error — `../`, an absolute
    /// path, and a link planted inside it — and nothing outside is read. The
    /// link case is Unix's (`std::os::unix::fs::symlink`); the test is Linux's.
    #[test]
    fn a_captains_drive_read_refuses_every_escape() {
        let dir = tempfile::tempdir().unwrap();
        let drive = dir.path().join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        // A real file one level above the Drive: the refusal is the guard's,
        // not a missing file's.
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "not the Captain's").unwrap();

        assert!(
            drive_read(&drive, "../outside.md").is_err(),
            "`..` out of the Drive is refused"
        );
        let absolute = outside.to_string_lossy().into_owned();
        assert!(
            drive_read(&drive, &absolute).is_err(),
            "an absolute path out of the Drive is refused"
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, drive.join("escape.md")).unwrap();
            let refused = drive_read(&drive, "escape.md");
            assert!(refused.is_err(), "a link pointing out of the Drive is refused");
            if let Ok(answer) = refused {
                assert_ne!(
                    answer["text"].as_str(),
                    Some("not the Captain's"),
                    "and the outside file's text never comes back"
                );
            }
        }
    }
}
