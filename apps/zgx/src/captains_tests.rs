//! SUR-83: the five Captain host actions are thin forwards — one RPC method
//! each, with the engine's own field names — and a refusal reaches the page as
//! the host's error instead of a swallowed success. SUR-94 adds the cascade's
//! three — `CaptainArchive`, `CaptainUnarchive`, `CaptainDelete` — the same
//! shape.
//!
//! The page is inline: it calls each through `ui.call` and reports the answer
//! or the refusal back through `test.record`, so what is asserted is what left
//! the host, not what a screen drew. `install_over` is the seam taken, so the
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
            // The engine's own `deny_unknown_fields` refusal when a caller
            // tries to write the registry's retired busy state. The bridge
            // must carry it byte for byte, not flatten it into a state word.
            if method == "PromoteCaptain" {
                return Err(
                    "bad params: unknown field `helm_busy`, expected `chatId` or `name`".to_owned(),
                );
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
            "PromoteCaptain=err:bad params: unknown field `helm_busy`, expected `chatId` or `name`".to_owned(),
            "DemoteCaptain=ok".to_owned(),
            "AddCrew=ok".to_owned(),
            "RemoveCrew=ok".to_owned(),
        ],
        "a helm_busy refusal reaches the page verbatim as the host's error"
    );
}

/// The words the bridge test pins are not a hand-written approximation:
/// this is the engine's own `deny_unknown_fields` answer to `helm_busy`,
/// over its real RPC service.
#[test]
fn the_engine_s_helm_busy_refusal_is_the_one_the_bridge_pins() {
    let rig = crate::helm_live::Rig::new();
    rig.create_chat("cap-a");
    let refused = rig
        .call(
            zeron_rpc::methods::PROMOTE_CAPTAIN,
            json!({ "chatId": "cap-a", "name": "Billing", "helm_busy": true }),
        )
        .expect_err("the registry has no helm_busy state to write");
    assert_eq!(
        refused,
        "bad params: unknown field `helm_busy`, expected `chatId` or `name`"
    );
}

/// What the page sees in `err.message` when the engine answers every
/// Captains call with `refusal`, recorded as the test's one line.
fn the_page_s_error_when_the_engine_refuses(refusal: &'static str) -> Vec<String> {
    let lines: Rc<RefCell<Vec<String>>> = Rc::default();
    let done = Rc::new(Cell::new(false));
    let call = move |_method: &'static str, _params: Json| -> crate::providers::McpCall {
        Box::pin(async move { Err(refusal.to_owned()) })
    };
    let page = r#"
ui.call("Captains", {}):after(function(_answer: any)
	ui.invoke("test.record", "ok")
	ui.invoke("test.done", nil)
end, function(err: any)
	ui.invoke("test.record", "err:" .. tostring(err.message))
	ui.invoke("test.done", nil)
end)

return ui.column({ w_full = true, h_full = true }, { ui.text({ text = "old engine" }) })
"#;

    let app = {
        let lines = Rc::clone(&lines);
        let done = Rc::clone(&done);
        TestApp::new(move |runtime| {
            let host = ScriptHost::inline(runtime, "captains_old_engine", page);
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
    assert!(done.get(), "the page never finished; what came back: {:?}", lines.borrow());
    lines.borrow().clone()
}

/// An engine from before the Captains RPCs is not a roster error: the page
/// gets the bridge's stable Helm-unavailable state, while any other refusal
/// keeps the engine's words.
#[test]
fn an_unknown_captains_method_is_the_helm_unavailable_state() {
    assert_eq!(
        the_page_s_error_when_the_engine_refuses("unknown method: Captains"),
        vec!["err:unsupported".to_owned()]
    );
}

/// A refusal that only *mentions* `unknown method` is not the older-engine
/// state: the engine's validation errors echo what they reject, so a Captain
/// name carrying those words must still arrive verbatim.
#[test]
fn a_refusal_mentioning_unknown_method_is_not_the_helm_unavailable_state() {
    let refusal =
        "invalid Captain name: \"the unknown method crew\" (allowed: 1-80 UTF-16 code units, no line breaks)";
    assert_eq!(
        the_page_s_error_when_the_engine_refuses(refusal),
        vec![format!("err:{refusal}")]
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

/// The Drive tab reads through the two host actions, not through
/// `drive_files`/`drive_read`: what the action answers is the contract. A
/// listing row is `{ name (Drive-relative), kind, size?, mtime? }` with an
/// RFC 3339 `mtime` as text; `CaptainDriveRead` answers `{ text, size }`, the
/// object the page's viewer reads as `file.text`, not a bare string — and a
/// path out of the Drive reaches the page as its error instead of a read.
#[test]
fn the_drive_actions_answer_the_page_with_the_listing_and_read_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let drive = dir.path().join("drive");
    std::fs::create_dir_all(drive.join("reports")).unwrap();
    std::fs::create_dir_all(drive.join("state")).unwrap();
    std::fs::write(drive.join("CREW.md"), "the crew").unwrap();
    std::fs::write(drive.join("reports/2026-11-23-a.md"), "a report").unwrap();
    std::fs::write(drive.join("state/needs-you.json"), "{}").unwrap();
    std::fs::write(dir.path().join("outside.md"), "not the Captain's").unwrap();

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
local function rows(files): string
	local out = {}
	for _, entry in files do
		table.insert(
			out,
			string.format("%s=%s/%s/%s", entry.name, entry.kind, type(entry.mtime), type(entry.size))
		)
	end
	return table.concat(out, ",")
end

ui.call("CaptainDrive", { chatId = "cap-a" }):after(function(answer: any)
	ui.invoke("test.record", "files:" .. rows(answer.files))
	ui.call("CaptainDriveRead", { chatId = "cap-a", path = "CREW.md" }):after(function(file: any)
		ui.invoke("test.record", string.format("read:%s/%s", tostring(file.text), type(file.size)))
		ui.call("CaptainDriveRead", { chatId = "cap-a", path = "../outside.md" }):after(function(_: any)
			ui.invoke("test.record", "escape=read")
			ui.invoke("test.done", nil)
		end, function(_err: any)
			ui.invoke("test.record", "escape=err")
			ui.invoke("test.done", nil)
		end)
	end, function(err: any)
		ui.invoke("test.record", "read-err:" .. tostring(err.message))
		ui.invoke("test.done", nil)
	end)
end, function(err: any)
	ui.invoke("test.record", "listing-err:" .. tostring(err.message))
	ui.invoke("test.done", nil)
end)

return ui.column({ w_full = true, h_full = true }, { ui.text({ text = "drive shapes" }) })
"#;

    let app = {
        let lines = Rc::clone(&lines);
        let done = Rc::clone(&done);
        TestApp::new(move |runtime| {
            let host = ScriptHost::inline(runtime, "captain_drive_shapes", page);
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
    assert!(done.get(), "the page never finished its calls; what came back: {:?}", lines.borrow());

    assert_eq!(
        *lines.borrow(),
        vec![
            "files:CREW.md=file/string/number,reports=directory/nil/nil,reports/2026-11-23-a.md=file/string/number"
                .to_owned(),
            "read:the crew/number".to_owned(),
            "escape=err".to_owned(),
        ],
        "the action's own answer: a relative name, \"directory\", RFC 3339 mtime text, {{ text, size }}"
    );
}

/// SUR-94: the archive cascade's three host actions are the same thin
/// forwards — `CaptainArchive`, `CaptainUnarchive`, `CaptainDelete` each take
/// `{ chatId }` and dispatch `ArchiveCaptain`, `UnarchiveCaptain`,
/// `DeleteCaptain` — and an engine refusal reaches the page as the host's
/// error, not a null or an empty roster.
#[test]
fn the_new_captain_actions_forward_to_their_own_rpc_methods() {
    let sent: Rc<RefCell<Vec<(String, Json)>>> = Rc::default();
    let lines: Rc<RefCell<Vec<String>>> = Rc::default();
    let done = Rc::new(Cell::new(false));

    let recording = Rc::clone(&sent);
    let call = move |method: &'static str, params: Json| -> crate::providers::McpCall {
        let sent = Rc::clone(&recording);
        Box::pin(async move {
            sent.borrow_mut().push((method.to_owned(), params));
            // The engine refuses a delete of a chat that is not a Captain, in
            // its own words. The bridge must not turn that into an empty
            // roster.
            if method == "DeleteCaptain" {
                return Err("bad params: chat-a is not a Captain on this device".to_owned());
            }
            Ok(json!({ "captains": [] }))
        })
    };

    let page = r#"
local CALLS = {
	{ name = "CaptainArchive", arg = { chatId = "chat-a" } },
	{ name = "CaptainUnarchive", arg = { chatId = "chat-a" } },
	{ name = "CaptainDelete", arg = { chatId = "chat-a" } },
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

return ui.column({ w_full = true, h_full = true }, { ui.text({ text = "captain archive" }) })
"#;

    let app = {
        let lines = Rc::clone(&lines);
        let done = Rc::clone(&done);
        TestApp::new(move |runtime| {
            let host = ScriptHost::inline(runtime, "captain_archive", page);
            crate::captains::install_over(&host, call);
            host.action("test.record", move |line| {
                lines.borrow_mut().push(crate::auth::text(&line));
            });
            host.action("test.done", move |_| done.set(true));
            host
        })
    };
    let mut app = app;

    // Deliver frames and real time until the page's three calls have all come
    // back (about four seconds at most).
    for _ in 0..2000 {
        app.frames(1);
        if done.get() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    let sent = sent.borrow();
    let got: Vec<(&str, &Json)> = sent.iter().map(|(method, params)| (method.as_str(), params)).collect();
    assert_eq!(
        got,
        vec![
            ("ArchiveCaptain", &json!({ "chatId": "chat-a" })),
            ("UnarchiveCaptain", &json!({ "chatId": "chat-a" })),
            ("DeleteCaptain", &json!({ "chatId": "chat-a" })),
        ],
        "each new action forwards to its own RPC method, with the engine's field names"
    );

    assert!(
        done.get(),
        "the page never finished its three calls; what came back: {:?}",
        lines.borrow()
    );
    assert_eq!(
        *lines.borrow(),
        vec![
            "CaptainArchive=ok".to_owned(),
            "CaptainUnarchive=ok".to_owned(),
            "CaptainDelete=err:bad params: chat-a is not a Captain on this device".to_owned(),
        ],
        "a refusal reaches the page as the host's error, in the engine's own words"
    );
}
