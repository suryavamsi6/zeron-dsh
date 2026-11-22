//! The Captains pages' host actions: the device-local roster, and the four
//! calls that change it.
//!
//! `ui.call` is a host action, not an engine RPC — asking it for `Captains`
//! answers "no host action named 'Captains'" — so each of these is one action
//! over one RPC method (`crates/rpc/src/lib.rs`: `Captains`, `PromoteCaptain`,
//! `DemoteCaptain`, `AddCrew`, `RemoveCrew`), under the name the page calls.
//! The roster is the engine's (`crates/engine/src/captains.rs`), device-local:
//! nothing here reads or writes `captains.json`, and no action invents a
//! second route to it.
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
    host.async_action("RemoveCrew", move |args| {
        let call = call.clone();
        let captain_id = args.field("captainId").map(text).unwrap_or_default();
        let chat_id = args.field("chatId").map(text).unwrap_or_default();
        async move {
            answer(call("RemoveCrew", json!({ "captainId": captain_id, "chatId": chat_id })).await)
        }
    });
}

/// The engine's answer as a script value, or its refusal as the host error.
/// A copy of `providers`' own, kept here because this module's slice of the
/// app is the one file SUR-83 was granted (`apps/zgx/src/captains.rs`) plus the
/// registration line, not `providers.rs`.
fn answer(reply: Result<Json, String>) -> Result<Value, String> {
    let json = reply?;
    let text = serde_json::to_string(&json).map_err(|error| error.to_string())?;
    Value::from_json(&text).ok_or_else(|| "the engine's answer didn't parse".to_string())
}
