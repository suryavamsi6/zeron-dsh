//! The provider page's host actions: the routes the gateway on this device
//! reports, the keys it keeps, and the ChatGPT sign-in's three steps.
//!
//! `ui.call` is a host action, not an engine RPC — asking it for `DshProviders`
//! answers "no host action named 'DshProviders'" — so each of these is one
//! action over one `Dsh*` method (`crates/engine/src/dsh_gateway.rs`), under
//! the name the page calls.

use serde_json::{Value as Json, json};
use zgx_script::{ScriptHost, Value};

use crate::auth::text;
use crate::engine;

pub fn install(host: &ScriptHost) {
    host.async_action("DshProviders", |_| async {
        answer(engine::get().call("DshProviders", json!({})).await)
    });
    host.async_action("DshCredentials", |args| {
        let refs = strings(args.field("refs"));
        async move {
            answer(engine::get().call("DshCredentials", json!({ "refs": refs })).await)
        }
    });
    host.async_action("DshSetCredential", |args| {
        let key_ref = args.field("ref").map(text).unwrap_or_default();
        let value = args.field("value").map(text).unwrap_or_default();
        async move {
            let params = json!({ "ref": key_ref, "value": value });
            answer(engine::get().call("DshSetCredential", params).await)
        }
    });
    host.async_action("DshChatgptStart", |_| async {
        answer(engine::get().call("DshChatgptStart", json!({})).await)
    });
    host.async_action("DshChatgptStatus", |_| async {
        answer(engine::get().call("DshChatgptStatus", json!({})).await)
    });
    host.async_action("DshChatgptCancel", |_| async {
        answer(engine::get().call("DshChatgptCancel", json!({})).await)
    });
}

/// The engine's answer as a script value: its JSON, or the words it refused
/// with. The page reads the same shape the gateway sends.
fn answer(reply: Result<Json, String>) -> Result<Value, String> {
    let json = reply?;
    let text = serde_json::to_string(&json).map_err(|error| error.to_string())?;
    Value::from_json(&text).ok_or_else(|| "the engine's answer didn't parse".to_string())
}

/// A list of strings from a script value (`{ refs = { "A", "B" } }`).
fn strings(value: Option<&Value>) -> Vec<String> {
    value.map(Value::list).unwrap_or_default().iter().map(text).collect()
}
