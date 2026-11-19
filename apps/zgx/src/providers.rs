//! The provider page's host actions: the routes the gateway on this device
//! reports, the keys it keeps, the ChatGPT sign-in's three steps, and the dsh
//! bundles it installs.
//!
//! `ui.call` is a host action, not an engine RPC — asking it for `DshProviders`
//! answers "no host action named 'DshProviders'" — so each of these is one
//! action over one `Dsh*` method (`crates/engine/src/dsh_gateway.rs`, and
//! `dsh_plugins.rs` for the bundles), under the name the page calls.

use serde_json::{Value as Json, json};
use zgx_script::{ScriptHost, Value};

use crate::auth::text;
use crate::engine;
use crate::read_aloud;

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
    // The dsh bundles in this computer's `sdk` profile, and the two
    // operations that change it (`crates/engine/src/dsh_plugins.rs`): the
    // list is a read of the profile's manifests, the two writes run
    // `dsh plugin` one at a time.
    host.async_action("DshPlugins", |_| async {
        answer(engine::get().call("DshPlugins", json!({})).await)
    });
    host.async_action("DshPluginAdd", |args| {
        let spec = args.field("spec").map(text).unwrap_or_default();
        // The go-ahead a build-script asking sends on its retry, for the
        // one package it named.
        let allow_build = args.field("allowBuild").map(text);
        async move {
            let mut params = json!({ "spec": spec });
            if let Some(allow_build) = allow_build {
                params["allowBuild"] = json!(allow_build);
            }
            answer(engine::get().call("DshPluginAdd", params).await)
        }
    });
    host.async_action("DshPluginRemove", |args| {
        let name = args.field("name").map(text).unwrap_or_default();
        async move {
            answer(engine::get().call("DshPluginRemove", json!({ "name": name })).await)
        }
    });
    // A plugin index's entries for the page's Discover section
    // (`crates/engine/src/dsh_plugin_index.rs`): the url through whole,
    // the page's locale with it for the index's own words.
    host.async_action("DshPluginIndex", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json())
            .map_err(|error| error.to_string())?;
        answer(engine::get().call("DshPluginIndex", params).await)
    });
    // A bundle's patch rows, and a row's switch written into the
    // profile's cordis.patch.yml. Args pass through whole: the engine
    // names what it takes, and a field added there reaches it.
    host.async_action("DshPluginRows", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json())
            .map_err(|error| error.to_string())?;
        answer(engine::get().call("DshPluginRows", params).await)
    });
    host.async_action("DshPluginSetRow", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json())
            .map_err(|error| error.to_string())?;
        answer(engine::get().call("DshPluginSetRow", params).await)
    });
    // The page's write: `{ ns, ops }` straight through, since the gateway
    // validates the ops itself (`settings/mutate`).
    host.async_action("DshMutateSettings", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json())
            .map_err(|error| error.to_string())?;
        answer(engine::get().call("DshMutateSettings", params).await)
    });
    // A DeepSeek answer read aloud (ui/screens/chat/aloud.luau): the
    // engine's `ReadAloud` turns its words into 24 kHz PCM, played here
    // (src/read_aloud.rs). The call lasts the read itself: it answers when
    // the sound ends — its words played out, a `StopReadAloud`, or a newer
    // read taking its place — so the button can stay Stop reading until then.
    host.async_action("ReadAloud", |args| {
        let ticket = read_aloud::ticket();
        let text = args.field("text").map(text).unwrap_or_default();
        async move {
            let reply = engine::get()
                .call("ReadAloud", json!({ "text": text }))
                .await?;
            if let Some(play) = read_aloud::play(&reply, ticket)? {
                play.done().await;
                read_aloud::release(&play);
            }
            Ok(Value::Nil)
        }
    });
    host.async_action("StopReadAloud", |_| async {
        read_aloud::stop();
        Ok(Value::Nil)
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
    // The engine's scheduled tasks (crates/engine/src/schedules.rs), for
    // Settings → Automation tasks: the list, and a task saved (`{task}`, an
    // empty `id` creating), deleted (`{id}`) or fired once now (`{id}`).
    // Args pass through whole: the engine names what each takes.
    host.async_action("Schedules", |_| async {
        answer(engine::get().call("Schedules", json!({})).await)
    });
    host.async_action("ScheduleSave", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("ScheduleSave", params).await)
    });
    host.async_action("ScheduleDelete", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("ScheduleDelete", params).await)
    });
    host.async_action("ScheduleRunNow", |args| async move {
        let params: Json = serde_json::from_str(&args.to_json()).map_err(|error| error.to_string())?;
        answer(engine::get().call("ScheduleRunNow", params).await)
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
