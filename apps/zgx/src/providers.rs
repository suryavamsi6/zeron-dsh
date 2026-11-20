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
    // The person's own MCP servers: one list on this device, read and
    // replaced whole by the settings page (engine `user_mcp`). The full
    // payload rides through: the engine names what it takes.
    install_mcp(host, |method, params| {
        let engine = engine::get();
        Box::pin(async move { engine.call(method, params).await })
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
/// One host action's engine call: the seam [`install_mcp`] takes, so the app
/// and an in-process test use the same action bodies.
pub(crate) type McpCall =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Json, String>>>>;

/// The MCP list's two host actions over `call`: this app's engine here, an
/// in-process `EngineCore` in `mcp_live_tests` (SUR-145). Same names, same
/// `args.to_json()` pass-through, same [`answer`].
/// What a JSON value is, for a diagnostic.
fn kind(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "a bool",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "an array",
        Json::Object(_) => "an object",
    }
}

/// `value` with every leaf under an `env` key replaced: a failure's shape
/// without a token in it.
fn shape(value: &Json) -> Json {
    match value {
        Json::Object(map) => Json::Object(
            map.iter()
                .map(|(key, value)| {
                    // `env`'s shape without a value: an object keeps its
                    // keys, an array its length (which is the bug's tell).
                    let value = if key == "env" {
                        match value {
                            Json::Object(map) => Json::Object(
                                map.keys()
                                    .map(|key| (key.clone(), Json::String("<redacted>".into())))
                                    .collect(),
                            ),
                            Json::Array(items) => Json::Array(
                                items.iter().map(|_| Json::String("<redacted>".into())).collect(),
                            ),
                            other => Json::String(format!("<env is {}>", kind(other))),
                        }
                    } else {
                        shape(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(shape).collect()),
        other => other.clone(),
    }
}

pub(crate) fn install_mcp<F>(host: &ScriptHost, call: F)
where
    F: Fn(&'static str, Json) -> McpCall + Clone + 'static,
{
    host.async_action("McpServers", {
        let call = call.clone();
        move |_| {
            let call = call.clone();
            async move { answer(call("McpServers", json!({})).await) }
        }
    });
    host.async_action("SetMcpServers", move |args| {
        let call = call.clone();
        let params: Result<Json, String> = serde_json::from_str(&args.to_json())
            .map_err(|error| error.to_string());
        async move {
            let mut params = params?;
            // The page's list crosses Luau as a table, and the engine's
            // contract is JSON: an empty table is `{}` where it wants `[]`,
            // and a list may arrive keyed "1".. rather than as an array.
            // Spell what `McpServer` names before the engine sees it, or a
            // server with no arguments (or an emptied list) is refused with
            // a shape error instead of saving (SUR-145).
            if let Some(list) = params.get_mut("servers") {
                if let Some(object) = list.as_object() {
                    let mut items: Vec<(u64, Json)> = object
                        .iter()
                        .filter_map(|(key, value)| {
                            key.parse::<u64>().ok().map(|index| (index, value.clone()))
                        })
                        .collect();
                    if items.len() == object.len() {
                        items.sort_by_key(|(index, _)| *index);
                        *list = Json::Array(items.into_iter().map(|(_, value)| value).collect());
                    }
                }
                if let Some(servers) = list.as_array_mut() {
                    for server in servers {
                        if let Some(args) = server.get_mut("args")
                            && args.as_object().is_some_and(|object| object.is_empty())
                        {
                            *args = Json::Array(Vec::new());
                        }
                        // An empty table is `[]` crossing Luau, and `env`
                        // is a map: a server with no environment (the
                        // common case) was refused with a shape error.
                        if let Some(env) = server.get_mut("env")
                            && env.as_array().is_some_and(|items| items.is_empty())
                        {
                            *env = Json::Object(serde_json::Map::new());
                        }
                    }
                }
            }
            let result = call("SetMcpServers", params.clone()).await;
            if result.is_err() {
                // The engine's words say what the shape should have been,
                // never which value. An env value can be a token, so only
                // the shape is printed: every leaf off `env` is replaced.
                eprintln!(
                    "SetMcpServers failed: {:?}\nshape: {}",
                    result.as_ref().err(),
                    serde_json::to_string(&shape(&params)).unwrap_or_default()
                );
            }
            answer(result)
        }
    });
}

fn answer(reply: Result<Json, String>) -> Result<Value, String> {
    let json = reply?;
    let text = serde_json::to_string(&json).map_err(|error| error.to_string())?;
    Value::from_json(&text).ok_or_else(|| "the engine's answer didn't parse".to_string())
}

/// A list of strings from a script value (`{ refs = { "A", "B" } }`).
fn strings(value: Option<&Value>) -> Vec<String> {
    value.map(Value::list).unwrap_or_default().iter().map(text).collect()
}
