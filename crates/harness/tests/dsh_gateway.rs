//! The gateway against a real `dsh web`: opt-in, because it installs nothing.
//!
//! Run with the pinned SDK present:
//! `ZERON_DEEPSEEK_SDK_DIR=$PWD/… cargo test -p zeron-harness --test dsh_gateway -- --ignored`
//!
//! It starts the service in a temporary home, so it never reads the machine's
//! own DeepSeek state, and it makes no model call.

use std::path::PathBuf;

use zeron_harness::deepseek::gateway::Gateway;

fn sdk_root() -> Option<PathBuf> {
    std::env::var_os("ZERON_DEEPSEEK_SDK_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[tokio::test]
#[ignore = "starts a real dsh web service from the pinned SDK"]
async fn the_provider_surface_answers_over_the_gateway() {
    let Some(sdk) = sdk_root() else {
        eprintln!("no ZERON_DEEPSEEK_SDK_DIR: nothing to start");
        return;
    };
    let home = tempfile::tempdir().expect("a scratch harness home");
    let gateway = Gateway::new(&sdk, home.path());

    let providers = gateway
        .call("llm/listConfigurableProviders", serde_json::json!({}))
        .await
        .expect("the provider catalog answers");
    let catalog = providers.as_array().expect("a catalog");
    assert!(
        catalog
            .iter()
            .any(|p| p["provider"] == "deepseek-official"),
        "the API-key route is offered: {providers}"
    );
    assert!(
        catalog.iter().all(|p| p["settingsNs"].is_string()),
        "every provider names the settings namespace that configures it"
    );

    let settings = gateway
        .call("settings/describe", serde_json::json!({}))
        .await
        .expect("settings answer");
    assert!(
        settings["namespaces"]
            .as_array()
            .is_some_and(|list| list.iter().any(|ns| ns["ns"] == "agent-default-model")),
        "the default model is a namespace DSH owns"
    );

    let account = gateway
        .call("account/getState", serde_json::json!({}))
        .await
        .expect("the account answers");
    assert!(
        account["status"].is_string(),
        "a scratch home has a state, signed out or not: {account}"
    );

    // Zeron's own ChatGPT sign-in plugin is loaded into this profile, so its
    // three methods are claimed by the gateway. Nothing here signs in: `start`
    // builds the authorize URL and binds the callback port, and `cancel`
    // releases it, which is the whole flow short of a browser.
    let status = gateway
        .call("zeron-chatgpt/status", serde_json::json!({}))
        .await
        .expect("the ChatGPT plugin answers");
    assert_eq!(status["authorized"], false, "a scratch home holds no grant");
    assert_eq!(status["running"], false);

    let started = gateway
        .call("zeron-chatgpt/start", serde_json::json!({}))
        .await
        .expect("the sign-in starts");
    let url = started["url"].as_str().expect("an authorize URL");
    assert!(
        url.starts_with("https://auth.openai.com/api/accounts/authorize?"),
        "the real authorize endpoint: {url}"
    );
    assert!(url.contains("code_challenge_method=S256"), "PKCE: {url}");
    assert!(
        url.contains("agent_name_hint=Zeron"),
        "the agent names itself: {url}"
    );
    assert!(
        url.contains("ext_agent_host_id=urn%3Auuid%3A"),
        "the installation id rides as a UUID: {url}"
    );
    assert!(
        url.contains("chatgpt.tokens.use.direct"),
        "the scope the grant is rejected without: {url}"
    );
    // The installation id is Zeron's own, kept under this home at 0600.
    let id = std::fs::read_to_string(home.path().join("zeron-installation-id"))
        .expect("the installation id is written");
    assert!(
        url.contains(id.trim()),
        "the URL carries the stored id: {url} vs {id}"
    );
    let running = gateway
        .call("zeron-chatgpt/status", serde_json::json!({}))
        .await
        .expect("status while running");
    assert_eq!(running["running"], true);
    let cancelled = gateway
        .call("zeron-chatgpt/cancel", serde_json::json!({}))
        .await
        .expect("the sign-in cancels");
    assert_eq!(cancelled["cancelled"], true);
    let after = gateway
        .call("zeron-chatgpt/status", serde_json::json!({}))
        .await
        .expect("status after cancel");
    assert_eq!(after["running"], false, "cancel leaves nothing running");

    // A credential written here lands in the store a session reads: the shim's
    // shared home points `credentials` at exactly this file, which is what
    // makes a provider configured in the app the provider the next run uses.
    gateway
        .call(
            "credentials/set",
            serde_json::json!({ "ref": "OPENCODE_API_KEY", "value": "synthetic-not-a-key" }),
        )
        .await
        .expect("the credential is stored");
    let store = std::fs::read_to_string(home.path().join(".credentials.yaml"))
        .expect("the shared credential store is the one the gateway writes");
    assert!(
        store.contains("OPENCODE_API_KEY"),
        "the ref is in the shared store: {store}"
    );
    let described = gateway
        .call(
            "credentials/describe",
            serde_json::json!({ "refs": ["OPENCODE_API_KEY"] }),
        )
        .await
        .expect("the credential describes");
    assert_eq!(
        described["OPENCODE_API_KEY"]["configured"], true,
        "and reads back as configured: {described}"
    );

    // The service outlives no test: shut it down and prove the next call
    // starts a fresh one rather than answering a dead port.
    gateway.shutdown().await;
    let again = gateway
        .call("account/getState", serde_json::json!({}))
        .await
        .expect("a restarted service answers");
    assert!(again["status"].is_string());
    gateway.shutdown().await;
}

/// Settings written through the gateway persist in the shared home across a
/// service restart — the proof behind "configured in the app, used by the
/// next run". Opt-in like the rest of this file: it starts a real `dsh web`
/// in a scratch home and makes no model call.
#[tokio::test]
#[ignore = "starts a real dsh web service from the pinned SDK"]
async fn settings_written_here_survive_a_service_restart() {
    let Some(sdk) = sdk_root() else {
        eprintln!("no ZERON_DEEPSEEK_SDK_DIR: nothing to start");
        return;
    };
    let home = tempfile::tempdir().expect("a scratch harness home");
    let gateway = Gateway::new(&sdk, home.path());

    // The default model namespace exists and starts empty of a pick.
    let before = gateway
        .call("settings/describe", serde_json::json!({}))
        .await
        .expect("settings answer");
    let default_ns = |value: &serde_json::Value| {
        value["namespaces"]
            .as_array()
            .and_then(|list| list.iter().find(|ns| ns["ns"] == "agent-default-model").cloned())
    };
    assert!(default_ns(&before).is_some(), "the default model ns exists: {before}");

    // Write the default model and an enabled provider through settings/mutate.
    gateway
        .call(
            "settings/mutate",
            serde_json::json!({
                "ns": "agent-default-model",
                "ops": [
                    { "op": "set", "path": ["provider"], "value": "deepseek-official" },
                    { "op": "set", "path": ["model"], "value": "deepseek-flash" },
                ],
            }),
        )
        .await
        .expect("the default model mutates");
    gateway
        .call(
            "settings/mutate",
            serde_json::json!({
                "ns": "llm-pi-ai",
                "ops": [{
                    "op": "set",
                    "path": ["providers", "opencode-go"],
                    "value": { "displayName": "OpenCode Go", "apiKeyEnv": "OPENCODE_API_KEY" },
                }],
            }),
        )
        .await
        .expect("the provider enables");

    let after = gateway
        .call("settings/describe", serde_json::json!({}))
        .await
        .expect("settings answer again");
    let default = default_ns(&after).expect("the default model ns");
    assert_eq!(
        default["value"]["model"], "deepseek-flash",
        "the default model reads back: {default}"
    );
    let pi_ai = after["namespaces"]
        .as_array()
        .and_then(|list| list.iter().find(|ns| ns["ns"] == "llm-pi-ai"))
        .expect("the llm-pi-ai ns");
    assert_eq!(
        pi_ai["value"]["providers"]["opencode-go"]["apiKeyEnv"], "OPENCODE_API_KEY",
        "the enabled provider reads back: {pi_ai}"
    );

    // Restart the service on the same home: both writes are still there.
    gateway.shutdown().await;
    let restarted = gateway
        .call("settings/describe", serde_json::json!({}))
        .await
        .expect("a restarted service answers");
    let default = default_ns(&restarted).expect("the default model ns after restart");
    assert_eq!(default["value"]["model"], "deepseek-flash");
    assert_eq!(default["value"]["provider"], "deepseek-official");
    let pi_ai = restarted["namespaces"]
        .as_array()
        .and_then(|list| list.iter().find(|ns| ns["ns"] == "llm-pi-ai"))
        .expect("the llm-pi-ai ns after restart");
    assert!(
        pi_ai["value"]["providers"]["opencode-go"].is_object(),
        "the provider is still enabled after restart: {pi_ai}"
    );
    gateway.shutdown().await;
}

/// A route pi-ai's catalog does not describe, declared through the same
/// `settings/mutate` the app writes, is the route DSH serves: it appears in the
/// catalog as declared, its profile reads back whole, and it is still there
/// after a service restart. The body below is the profile
/// `zeron_engine::dsh_providers::declare_route_params` builds — that module's
/// unit test pins its exact shape — and the ops are the store's own
/// vocabulary (`set`/`unset`), not JSON-patch's.
#[tokio::test]
#[ignore = "starts a real dsh web service from the pinned SDK"]
async fn a_declared_route_appears_in_the_catalog_and_survives_a_restart() {
    let Some(sdk) = sdk_root() else {
        eprintln!("no ZERON_DEEPSEEK_SDK_DIR: nothing to start");
        return;
    };
    let home = tempfile::tempdir().expect("a scratch harness home");
    let gateway = Gateway::new(&sdk, home.path());
    let route = "zeron-declared-probe";

    let profile = serde_json::json!({
        "displayName": "Zeron declared probe",
        "apiKeyEnv": "ZERON_DECLARED_PROBE_API_KEY",
        "api": "openai-completions",
        "baseURL": "http://127.0.0.1:41999/openai/v1",
        "headers": { "x-zeron-probe": "declared" },
        "compat": {
            "supportsStore": false,
            "supportsDeveloperRole": false,
            "supportsStrictMode": true,
            "maxTokensField": "max_tokens",
            "requiresReasoningContentOnAssistantMessages": true,
            "thinkingFormat": "deepseek",
        },
        "models": [
            { "id": "probe-flash", "name": "Probe Flash", "contextWindow": 1_000_000, "maxTokens": 64_000, "input": ["text", "image"] },
            { "id": "probe-mini", "contextWindow": 128_000, "maxTokens": 8_192, "input": ["text"] },
        ],
    });
    gateway
        .call(
            "settings/mutate",
            serde_json::json!({
                "ns": "llm-pi-ai",
                "ops": [{ "op": "set", "path": ["providers", route], "value": profile }],
            }),
        )
        .await
        .expect("the declaration is accepted");

    let listed = gateway
        .call("llm/listConfigurableProviders", serde_json::json!({}))
        .await
        .expect("the catalog answers");
    let entry = listed
        .as_array()
        .and_then(|catalog| catalog.iter().find(|p| p["provider"] == route))
        .unwrap_or_else(|| panic!("the declared route is in the catalog: {listed}"));
    assert_eq!(entry["declared"], true, "and is marked as declared: {entry}");
    assert_eq!(entry["displayName"], "Zeron declared probe");
    assert_eq!(entry["settingsPath"], serde_json::json!(["providers", route]));

    // The whole profile is what DSH read back, defaults filled in around it.
    let settings = gateway
        .call("settings/describe", serde_json::json!({}))
        .await
        .expect("settings answer");
    let stored = &settings["namespaces"]
        .as_array()
        .and_then(|list| list.iter().find(|ns| ns["ns"] == "llm-pi-ai"))
        .expect("the llm-pi-ai ns")["value"]["providers"][route];
    assert_eq!(stored["api"], "openai-completions", "{stored}");
    assert_eq!(stored["baseURL"], "http://127.0.0.1:41999/openai/v1");
    assert_eq!(stored["apiKeyEnv"], "ZERON_DECLARED_PROBE_API_KEY");
    assert_eq!(stored["headers"]["x-zeron-probe"], "declared");
    assert_eq!(stored["compat"]["maxTokensField"], "max_tokens");
    assert_eq!(stored["compat"]["thinkingFormat"], "deepseek");
    assert_eq!(stored["models"][0]["input"], serde_json::json!(["text", "image"]));
    assert_eq!(
        stored["models"][1]["maxTokens"], 8_192,
        "a model's output limit is its request cap: {stored}"
    );

    // The token meter answers for this home: no sessions yet, and a missing
    // session's projections are null rather than an invented zero.
    let sessions = gateway
        .call("session/list", serde_json::json!({ "_request": {} }))
        .await
        .expect("the session list answers");
    assert!(
        sessions["items"].is_array(),
        "the meter's own list shape: {sessions}"
    );
    let projections = gateway
        .call(
            "session/projections",
            serde_json::json!({ "request": { "sessionId": "ses_probe_missing" } }),
        )
        .await
        .expect("a projection read answers");
    assert!(projections.is_null(), "no session, no value: {projections}");

    // Restart on the same home: the declaration is still the route DSH serves.
    gateway.shutdown().await;
    let restarted = gateway
        .call("llm/listConfigurableProviders", serde_json::json!({}))
        .await
        .expect("a restarted service answers");
    assert!(
        restarted
            .as_array()
            .is_some_and(|catalog| catalog.iter().any(|p| p["provider"] == route)),
        "the declared route survived the restart: {restarted}"
    );

    // Removal is the off switch: the path is unset and the catalog forgets it.
    gateway
        .call(
            "settings/mutate",
            serde_json::json!({
                "ns": "llm-pi-ai",
                "ops": [{ "op": "unset", "path": ["providers", route] }],
            }),
        )
        .await
        .expect("the route is removed");
    let after = gateway
        .call("llm/listConfigurableProviders", serde_json::json!({}))
        .await
        .expect("the catalog answers after removal");
    assert!(
        after
            .as_array()
            .is_some_and(|catalog| catalog.iter().all(|p| p["provider"] != route)),
        "a removed route is gone from the catalog: {after}"
    );
    gateway.shutdown().await;
}

#[tokio::test]
#[ignore = "starts a real dsh web service from the pinned SDK"]
async fn chatgpt_account_remotes_bind_on_the_gateway() {
    let Some(sdk) = sdk_root() else { return };
    let home = tempfile::tempdir().unwrap();
    let gateway = Gateway::new(&sdk, home.path());
    let accounts = gateway.call("zeron-chatgpt/accounts", serde_json::json!({})).await.unwrap();
    assert_eq!(accounts["provider"], "openai");
    assert_eq!(accounts["accounts"], serde_json::json!([]));
    let status = gateway.call("zeron-chatgpt/status", serde_json::json!({"provider": "openai-codex"})).await.unwrap();
    assert_eq!(status["provider"], "openai-codex");
    assert_eq!(status["authorized"], false);
    let codex = gateway.call("zeron-chatgpt/accounts", serde_json::json!({"provider": "openai-codex"})).await.unwrap();
    assert_eq!(codex["provider"], "openai-codex");
    assert_eq!(codex["accounts"], serde_json::json!([]));
    let select = gateway.call("zeron-chatgpt/select", serde_json::json!({ "accountId": "missing" })).await.unwrap_err().to_string();
    assert!(select.contains("ChatGPT account was not found"), "{select}");
    let reset = gateway.call("zeron-chatgpt/useReset", serde_json::json!({
        "accountId": "missing", "creditId": "missing", "idempotencyKey": "synthetic", "provider": "openai-codex"
    })).await.unwrap_err().to_string();
    assert!(reset.contains("Account or reset is unavailable"), "{reset}");
    let started = gateway.call("zeron-chatgpt/start", serde_json::json!({"provider": "openai-codex"})).await.unwrap();
    assert!(started["url"].as_str().unwrap().starts_with("https://auth.openai.com/oauth/authorize?"));
    let cancel = gateway.call("zeron-chatgpt/cancel", serde_json::json!({"provider": "openai-codex"})).await.unwrap();
    assert_eq!(cancel["cancelled"], true);
    gateway.shutdown().await;
}
