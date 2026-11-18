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
    // The page's write: `{ ns, ops }` straight through, since the gateway
    // validates the ops itself (`settings/mutate`).
    host.async_action("DshMutateSettings", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json())
            .map_err(|error| error.to_string())?;
        answer(engine::get().call("DshMutateSettings", params).await)
    });
    // The routes' models, for the Providers page's Models section: the
    // engine's own list, which already carries each model's route.
    host.async_action("ListModels", |args| async move {
        let harness = args
            .field("harness")
            .map(text)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "deepseek".to_string());
        answer(engine::get().call("ListModels", json!({ "harness": harness })).await)
    });
    host.async_action("DshChatgptStart", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshChatgptStart", params).await)
    });
    host.async_action("DshChatgptStatus", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshChatgptStatus", params).await)
    });
    host.async_action("DshChatgptSelect", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshChatgptSelect", params).await)
    });
    host.async_action("DshChatgptAccounts", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshChatgptAccounts", params).await)
    });
    host.async_action("DshChatgptRemoveAccount", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshChatgptRemoveAccount", params).await)
    });
    host.async_action("DshChatgptUseReset", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshChatgptUseReset", params).await)
    });
    host.async_action("DshChatgptCancel", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshChatgptCancel", params).await)
    });
    host.async_action("DshProviderAuthStatus", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshProviderAuthStatus", params).await)
    });
    host.async_action("DshProviderAuthStart", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshProviderAuthStart", params).await)
    });
    host.async_action("DshProviderAuthCancel", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshProviderAuthCancel", params).await)
    });
    host.async_action("DshProviderAuthRespond", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshProviderAuthRespond", params).await)
    });
    host.async_action("DshProviderAuthSelect", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshProviderAuthSelect", params).await)
    });
    host.async_action("DshProviderAuthRemove", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("DshProviderAuthRemove", params).await)
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
