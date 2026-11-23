//! SUR-83: the five Captain host actions are thin forwards — one RPC method
//! each, with the engine's own field names — and a refusal reaches the page as
//! the host's error instead of a swallowed success.
//!
//! The page is inline: it calls all five through `ui.call` and reports each
//! answer back through `test.record`, so what is asserted is what left the
//! host, not what a screen drew. `install_over` is the seam taken, so the
//! bridge's own forwarding is what runs.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value as Json, json};
use zgx_script::testing::TestApp;
use zgx_script::{ScriptHost, Value};

/// The page: each call in turn, its answer or its refusal reported back.
const PAGE: &str = r#"
local CALLS = {
	{ name = "Captains" },
	{ name = "PromoteCaptain", arg = { chatId = "chat-a", name = "Billing" } },
	{ name = "DemoteCaptain", arg = { chatId = "chat-a" } },
	{ name = "AddCrew", arg = { captainId = "chat-a", chatId = "chat-b" } },
	{ name = "RemoveCrew", arg = { captainId = "chat-a", chatId = "chat-b" } },
}

local function step(i: number)
	if i > #CALLS then
		ui.invoke("test.done", nil)
		return
	end
	local call = CALLS[i]
	local promise = if call.arg then ui.call(call.name, call.arg) else ui.call(call.name)
	promise:after(function(_answer: any)
		ui.invoke("test.record", call.name .. "=ok")
		step(i + 1)
	end, function(err: any)
		ui.invoke("test.record", call.name .. "=err:" .. tostring(err.message))
		step(i + 1)
	end)
end

step(1)

return ui.column({ w_full = true, h_full = true }, { ui.text({ text = "captains host" }) })
"#;

#[test]
fn every_captain_action_forwards_to_its_own_rpc_method_and_surfaces_a_refusal() {
    let sent: Rc<RefCell<Vec<(String, Json)>>> = Rc::default();
    let lines: Rc<RefCell<Vec<String>>> = Rc::default();
    let done = Rc::new(Cell::new(false));

    let recording = Rc::clone(&sent);
    let call = move |method: &'static str, params: Json| -> crate::providers::McpCall {
        let sent = Rc::clone(&recording);
        Box::pin(async move {
            sent.borrow_mut().push((method.to_owned(), params));
            // The engine refuses a demote of a chat that is not a Captain, in
            // its own words (`crates/engine/src/captains.rs`). The bridge must
            // not turn that into an empty roster.
            if method == "DemoteCaptain" {
                return Err("bad params: chat-a is not a Captain on this device".to_owned());
            }
            Ok(json!({ "captains": [] }))
        })
    };

    let app = {
        let lines = Rc::clone(&lines);
        let done = Rc::clone(&done);
        TestApp::new(move |runtime| {
            let host = ScriptHost::inline(runtime, "captains_host", PAGE);
            crate::captains::install_over(&host, call);
            host.action("test.record", move |line| {
                lines.borrow_mut().push(crate::auth::text(&line));
            });
            host.action("test.done", move |_| done.set(true));
            host
        })
    };
    let mut app = app;

    // Deliver frames and real time until the page's five calls have all come
    // back (about four seconds at most).
    for _ in 0..2000 {
        app.frames(1);
        if done.get() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        done.get(),
        "the page never finished its five calls; what came back: {:?}",
        lines.borrow()
    );

    let sent = sent.borrow();
    let got: Vec<(&str, &Json)> = sent.iter().map(|(method, params)| (method.as_str(), params)).collect();
    assert_eq!(
        got,
        vec![
            ("Captains", &json!({})),
            ("PromoteCaptain", &json!({ "chatId": "chat-a", "name": "Billing" })),
            ("DemoteCaptain", &json!({ "chatId": "chat-a" })),
            ("AddCrew", &json!({ "captainId": "chat-a", "chatId": "chat-b" })),
            ("RemoveCrew", &json!({ "captainId": "chat-a", "chatId": "chat-b" })),
        ],
        "each action forwards to its own RPC method, with the engine's field names"
    );

    assert_eq!(
        *lines.borrow(),
        vec![
            "Captains=ok".to_owned(),
            "PromoteCaptain=ok".to_owned(),
            "DemoteCaptain=err:bad params: chat-a is not a Captain on this device".to_owned(),
            "AddCrew=ok".to_owned(),
            "RemoveCrew=ok".to_owned(),
        ],
        "a refusal reaches the page as the host's error, in the engine's own words"
    );
}

/// The Digest tab's one read: the roster's `drivePath` is the only path it
/// takes, the Drive's three files come back as they are on disk, a linked
/// file out of the Drive is not read, and a chat that is not a Captain is
/// refused in the page's error instead of answering an empty Drive.
#[test]
fn the_digest_action_reads_only_the_roster_captain_s_drive() {
    let dir = tempfile::tempdir().unwrap();
    let drive = dir.path().join("drive");
    std::fs::create_dir_all(drive.join("state")).unwrap();
    std::fs::create_dir_all(drive.join("reports")).unwrap();
    std::fs::write(drive.join("DIGEST.md"), "Billing: 2 asks.").unwrap();
    std::fs::write(
        drive.join("state/needs-you.json"),
        r#"{"version":1,"updatedAt":"2026-11-22T01:00:00Z","items":["one","two"]}"#,
    )
    .unwrap();
    // Two reports, the newer one second on disk: the tab reads them newest
    // first, so what it shows does not depend on the folder's order.
    let alpha = drive.join("reports/alpha.md");
    let beta = drive.join("reports/beta.md");
    std::fs::write(&alpha, "alpha report").unwrap();
    std::fs::write(&beta, "beta report").unwrap();
    let now = std::time::SystemTime::now();
    std::fs::File::options().write(true).open(&alpha).unwrap().set_modified(now - std::time::Duration::from_secs(60)).unwrap();
    std::fs::File::options().write(true).open(&beta).unwrap().set_modified(now).unwrap();
    // A link planted in the Drive that points out of it: never read.
    #[cfg(unix)]
    {
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "not the Captain's").unwrap();
        std::os::unix::fs::symlink(&outside, drive.join("reports/escape.md")).unwrap();
    }

    let lines: Rc<RefCell<Vec<String>>> = Rc::default();
    let done = Rc::new(Cell::new(false));
    let roster = json!({ "captains": [
        { "chatId": "cap-a", "name": "Billing", "drivePath": drive.to_string_lossy() },
    ] });
    let call = move |method: &'static str, _params: Json| -> crate::providers::McpCall {
        let roster = roster.clone();
        Box::pin(async move {
            match method {
                "Captains" => Ok(roster),
                other => Err(format!("unexpected method {other}")),
            }
        })
    };

    let page = r#"
local CHATS = { "cap-a", "chat-z" }

local function summarise(a: any): string
	local items = table.concat((a.needsYou and a.needsYou.items) or {}, "+")
	local reports = {}
	for _, report in a.reports or {} do
		table.insert(reports, report.name .. ":" .. report.text)
	end
	return string.format("digest=%s items=%s reports=%s", tostring(a.digest), items, table.concat(reports, ";"))
end

local function step(i: number)
	if i > #CHATS then
		ui.invoke("test.done", nil)
		return
	end
	ui.call("CaptainDrive", { chatId = CHATS[i] }):after(function(answer: any)
		ui.invoke("test.record", summarise(answer))
		step(i + 1)
	end, function(err: any)
		ui.invoke("test.record", "err:" .. tostring(err.message))
		step(i + 1)
	end)
end

step(1)

return ui.column({ w_full = true, h_full = true }, { ui.text({ text = "captain drive" }) })
"#;

    let app = {
        let lines = Rc::clone(&lines);
        let done = Rc::clone(&done);
        TestApp::new(move |runtime| {
            let host = ScriptHost::inline(runtime, "captain_drive", page);
            crate::captains::install_over(&host, call);
            host.action("test.record", move |line| {
                lines.borrow_mut().push(crate::auth::text(&line));
            });
            host.action("test.done", move |_| done.set(true));
            host
        })
    };
    let mut app = app;
    for _ in 0..2000 {
        app.frames(1);
        if done.get() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(done.get(), "the page never finished its reads; what came back: {:?}", lines.borrow());

    assert_eq!(
        *lines.borrow(),
        vec![
            "digest=Billing: 2 asks. items=one+two reports=beta.md:beta report;alpha.md:alpha report".to_owned(),
            "err:That chat is not a Captain on this device".to_owned(),
        ],
        "the Drive's files, newest report first, and a refusal for a chat that is not a Captain"
    );
}

/// One `CaptainDrive` read against a roster holding a single Captain whose
/// `drivePath` is `drive`; what the page recorded comes back in order.
fn drive_listing(drive: &std::path::Path) -> Vec<String> {
    const PAGE: &str = r#"
local function encode(a: any): string
	local out = {}
	for _, entry in a.files or {} do
		table.insert(out, entry.name .. "|" .. entry.kind .. "|" .. entry.path)
	end
	return table.concat(out, ";")
end

ui.call("CaptainDrive", { chatId = "cap-a" }):after(function(answer: any)
	ui.invoke("test.record", encode(answer))
	ui.invoke("test.done", nil)
end, function(err: any)
	ui.invoke("test.record", "err:" .. tostring(err.message))
	ui.invoke("test.done", nil)
end)

return ui.column({ w_full = true, h_full = true }, { ui.text({ text = "drive files" }) })
"#;

    let lines: Rc<RefCell<Vec<String>>> = Rc::default();
    let done = Rc::new(Cell::new(false));
    let roster = json!({ "captains": [
        { "chatId": "cap-a", "name": "Billing", "drivePath": drive.to_string_lossy() },
    ] });
    let call = move |method: &'static str, _params: Json| -> crate::providers::McpCall {
        let roster = roster.clone();
        Box::pin(async move {
            match method {
                "Captains" => Ok(roster),
                other => Err(format!("unexpected method {other}")),
            }
        })
    };

    let app = {
        let lines = Rc::clone(&lines);
        let done = Rc::clone(&done);
        TestApp::new(move |runtime| {
            let host = ScriptHost::inline(runtime, "captain_files", PAGE);
            crate::captains::install_over(&host, call);
            host.action("test.record", move |line| {
                lines.borrow_mut().push(crate::auth::text(&line));
            });
            host.action("test.done", move |_| done.set(true));
            host
        })
    };
    let mut app = app;
    for _ in 0..2000 {
        app.frames(1);
        if done.get() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(done.get(), "the page never finished its read; what came back: {:?}", lines.borrow());
    let recorded = lines.borrow().clone();
    recorded
}

/// SUR-92: the Drive tab lists the Captain's files by a path relative to the
/// Drive — the roster's `drivePath` is the only root, and a link planted in
/// the Drive is not a way to show the page a file outside it.
#[test]
fn the_drive_listing_is_relative_and_never_lists_a_way_out() {
    let dir = tempfile::tempdir().unwrap();
    let drive = dir.path().join("drive");
    std::fs::create_dir_all(drive.join("reports/sub")).unwrap();
    std::fs::create_dir_all(drive.join("handoffs")).unwrap();
    std::fs::create_dir_all(drive.join("state")).unwrap();
    std::fs::write(drive.join("DIGEST.md"), "digest").unwrap();
    std::fs::write(drive.join("reports/a.md"), "a").unwrap();
    std::fs::write(drive.join("reports/sub/b.md"), "b").unwrap();
    std::fs::write(drive.join("handoffs/h.md"), "h").unwrap();
    std::fs::write(drive.join("state/needs-you.json"), "{}").unwrap();
    // A link planted in the Drive that points out of it: it is not the
    // Captain's, so the page must not be offered it.
    #[cfg(unix)]
    {
        let outside_file = dir.path().join("outside.md");
        std::fs::write(&outside_file, "not the Captain's").unwrap();
        std::os::unix::fs::symlink(&outside_file, drive.join("escape.md")).unwrap();
        let outside_dir = dir.path().join("outside-dir");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::os::unix::fs::symlink(&outside_dir, drive.join("escape-dir")).unwrap();
    }

    let recorded = drive_listing(&drive);
    assert_eq!(recorded.len(), 1, "one read, one answer: {recorded:?}");
    let answer = &recorded[0];
    assert!(!answer.starts_with("err:"), "the read was refused: {answer}");
    assert_eq!(
        answer,
        "DIGEST.md|file|DIGEST.md;handoffs|dir|handoffs;h.md|file|handoffs/h.md;\
reports|dir|reports;a.md|file|reports/a.md;sub|dir|reports/sub;b.md|file|reports/sub/b.md",
        "the Drive's files, relative and sorted"
    );
    for entry in answer.split(';') {
        let mut fields = entry.split('|');
        let name = fields.next().unwrap();
        let kind = fields.next().unwrap();
        let path = fields.next().unwrap();
        assert!(kind == "file" || kind == "dir", "unknown kind in {entry}");
        assert!(!name.contains('/'), "a name carried a path: {entry}");
        assert!(!path.starts_with('/') && !path.starts_with('\\'), "an absolute path was listed: {entry}");
        assert!(!path.split('/').any(|part| part == ".."), "a parent path was listed: {entry}");
        assert!(
            !path.starts_with(drive.to_string_lossy().as_ref()),
            "the listing carried the Drive's own path: {entry}"
        );
    }
}

/// SUR-92: `state/` is machine-only — the engine's Needs-you snapshot lives
/// there — so the page never sees it, while the rest of the Drive is listed.
#[test]
fn the_drive_listing_hides_the_machine_only_state_folder() {
    let dir = tempfile::tempdir().unwrap();
    let drive = dir.path().join("drive");
    std::fs::create_dir_all(drive.join("state")).unwrap();
    std::fs::create_dir_all(drive.join("reports")).unwrap();
    std::fs::write(drive.join("DIGEST.md"), "digest").unwrap();
    std::fs::write(drive.join("state/needs-you.json"), "{}").unwrap();
    std::fs::write(drive.join("reports/a.md"), "a").unwrap();

    let recorded = drive_listing(&drive);
    assert_eq!(recorded.len(), 1, "one read, one answer: {recorded:?}");
    let answer = &recorded[0];
    assert!(answer.contains("DIGEST.md|file|DIGEST.md"), "the listing did not reach the Drive: {answer}");
    assert!(answer.contains("a.md|file|reports/a.md"), "the reports folder was not listed: {answer}");
    assert!(!answer.contains("state"), "the machine-only state folder reached the page: {answer}");
}
