//! The Captains pages' host actions: the device-local roster, the four calls
//! that change it, and one read of a Captain's Drive for the Digest tab.
//!
//! `ui.call` is a host action, not an engine RPC — asking it for `Captains`
//! answers "no host action named 'Captains'" — so each of these is one action
//! over one RPC method (`crates/rpc/src/lib.rs`: `Captains`, `PromoteCaptain`,
//! `DemoteCaptain`, `AddCrew`, `RemoveCrew`), under the name the page calls.
//! The roster is the engine's (`crates/engine/src/captains.rs`), device-local:
//! nothing here reads or writes `captains.json`, and no action invents a
//! second route to it.
//!
//! `CaptainDrive` is not an RPC: the Drive is a folder on this device
//! (`crates/engine/src/captains.rs`'s `drive_dir`), and the roster's
//! `drivePath` is the only path it reads.
//!
//! Field names are the engine's, unchanged: promoting takes `{chatId, name}`,
//! demoting `{chatId}`, and the crew calls `{captainId, chatId}`. An error the
//! engine answers — a busy chat, a crew member that cannot be promoted, a
//! refusal because the roster is another device's — reaches the page as the
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

/// The five over `call` — the same seam [`crate::providers::install_mcp`]
/// takes, so a test can drive them against an engine on memory IPC instead of
/// the process-wide one, and can record which method each action dispatches.
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
    // A Captain's Drive, for the Digest tab: `{ chatId }` ->
    //   { digest = <DIGEST.md's text>?, needsYou = { version, updatedAt,
    //     items }?, reports = { { name, text } } }
    // The roster decides whose Drive it is: a chat that is not a Captain on
    // this device is refused, and none of the three is a path the page gave.
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
                let answer = json!({ "digest": digest, "needsYou": needs_you, "reports": reports });
                Value::from_json(&serde_json::to_string(&answer).map_err(|error| error.to_string())?)
                    .ok_or_else(|| "the Drive's answer didn't parse".to_string())
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
