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
                    { "op": "add", "path": ["provider"], "value": "deepseek-official" },
                    { "op": "add", "path": ["model"], "value": "deepseek-flash" },
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
                    "op": "add",
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
