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
