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
