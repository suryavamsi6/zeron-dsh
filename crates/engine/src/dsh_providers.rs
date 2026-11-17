//! Typed shapes for DSH's gateway surface, parsed for the app's settings UI.
//!
//! [`crate::dsh_gateway`] forwards the calls; this module reads and builds
//! their argument and answer shapes, so the UI never guesses at a wire field.
//! Every parser is defensive: a field DSH renames degrades to `None`, never
//! to a wrong answer.

use serde_json::{json, Value};

/// The settings namespace that holds the default model DSH runs a session
/// with when the app sends none.
pub const DEFAULT_MODEL_NS: &str = "agent-default-model";

/// One configurable provider route DSH's catalog offers
/// (`llm/listConfigurableProviders`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProvider {
    /// The route id (`deepseek-official`, `opencode-go`, ...).
    pub id: String,
    pub display_name: String,
    /// The settings namespace that configures this route.
    pub settings_ns: String,
    /// The env-shaped credential ref (`apiKeyEnv`) an API-key route reads,
    /// e.g. `DEEPSEEK_API_KEY` or `OPENCODE_API_KEY`.
    pub api_key_env: Option<String>,
    /// Whether the route signs in through DSH's own account flow.
    pub account: bool,
    /// Whether the route is the ChatGPT subscription, served by Zeron's own
    /// plugin on the same gateway (`zeron-chatgpt/*`).
    pub chatgpt: bool,
}

impl GatewayProvider {
    /// The credential ref to describe/set/unset for this route.
    pub fn credential_ref(&self) -> Option<&str> {
        self.api_key_env.as_deref()
    }
}

/// Parse the `llm/listConfigurableProviders` answer. Entries without a
/// `provider` or `settingsNs` are skipped, not guessed at.
pub fn parse_provider_catalog(value: &Value) -> Vec<GatewayProvider> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = entry["provider"].as_str()?.to_string();
            let settings_ns = entry["settingsNs"].as_str()?.to_string();
            let api_key_env = entry["apiKeyEnv"].as_str().map(str::to_string);
            let chatgpt = entry["auth"].as_str() == Some("chatgpt") || id.contains("chatgpt");
            let account = !chatgpt
                && (entry["account"].as_bool() == Some(true)
                    || entry["auth"].as_str() == Some("account")
                    || id.contains("deepseek"));
            Some(GatewayProvider {
                display_name: entry["displayName"]
                    .as_str()
                    .unwrap_or(&id)
                    .to_string(),
                id,
                settings_ns,
                api_key_env,
                account,
                chatgpt,
            })
        })
        .collect()
}

/// Whether `ref` reads back as configured from a `credentials/describe`
/// answer (`{REF: {configured: true, ...}}`).
pub fn credential_configured(describe: &Value, key_ref: &str) -> bool {
    describe[key_ref]["configured"].as_bool().unwrap_or(false)
}

/// The `credentials/describe` params for a set of refs.
pub fn describe_credentials_params(refs: &[&str]) -> Value {
    json!({ "refs": refs })
}

/// The `credentials/set` params storing `value` under `ref`.
pub fn set_credential_params(key_ref: &str, value: &str) -> Value {
    json!({ "ref": key_ref, "value": value })
}

/// The `credentials/unset` params for `ref`.
pub fn unset_credential_params(key_ref: &str) -> Value {
    json!({ "ref": key_ref })
}

/// The DeepSeek account's state from `account/getState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountState {
    pub status: String,
    pub signed_in: bool,
}

pub fn parse_account_state(value: &Value) -> AccountState {
    let status = value["status"].as_str().unwrap_or("unknown").to_string();
    let signed_in = value["signedIn"].as_bool().unwrap_or(status == "signed-in");
    AccountState { status, signed_in }
}

/// The sign-in start answer: the URL to open and the attempt to cancel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInStart {
    pub url: Option<String>,
    pub attempt_id: Option<String>,
}

pub fn parse_sign_in_start(value: &Value) -> SignInStart {
    SignInStart {
        url: value["url"].as_str().map(str::to_string),
        attempt_id: value["attemptId"].as_str().map(str::to_string),
    }
}

/// The ChatGPT plugin's `zeron-chatgpt/status` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptStatus {
    pub authorized: bool,
    pub running: bool,
    pub url: Option<String>,
}

pub fn parse_chatgpt_status(value: &Value) -> ChatGptStatus {
    ChatGptStatus {
        authorized: value["authorized"].as_bool().unwrap_or(false),
        running: value["running"].as_bool().unwrap_or(false),
        url: value["url"].as_str().map(str::to_string),
    }
}

/// The authorize URL from `zeron-chatgpt/start`.
pub fn parse_chatgpt_start(value: &Value) -> Option<String> {
    value["url"].as_str().map(str::to_string)
}

/// One model in a catalog answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: String,
    pub label: String,
}

/// Parse a model list: a bare array, or an object holding one under
/// `models`/`items`/`data`.
pub fn parse_models(value: &Value) -> Vec<ModelChoice> {
    let array = value.as_array().or_else(|| {
        ["models", "items", "data"]
            .iter()
            .find_map(|key| value[key].as_array())
    });
    array
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = entry["id"].as_str()?.to_string();
            let label = entry["name"]
                .as_str()
                .or_else(|| entry["label"].as_str())
                .or_else(|| entry["displayName"].as_str())
                .unwrap_or(&id)
                .to_string();
            Some(ModelChoice { id, label })
        })
        .collect()
}

/// The namespaces a `settings/describe` answer holds: `ns` -> its value.
pub fn settings_namespaces(value: &Value) -> Vec<(&str, &Value)> {
    value["namespaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|ns| Some((ns["ns"].as_str()?, ns.get("value").unwrap_or(&Value::Null))))
        .collect()
}

/// The models a settings namespace carries, read offline from the store
/// (no provider call): the first array of `{id, ...}` objects under any
/// top-level key of the ns value.
pub fn models_in_ns(settings: &Value, ns: &str) -> Vec<ModelChoice> {
    let Some((_, value)) = settings_namespaces(settings).into_iter().find(|(name, _)| *name == ns)
    else {
        return Vec::new();
    };
    value
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(_, field)| parse_models(field))
        .collect()
}

/// The `llm/discoverModels` params for a route's settings namespace. This is
/// the live, provider-contacting call.
pub fn discover_models_params(settings_ns: &str) -> Value {
    json!({ "settingsNs": settings_ns })
}

/// The default model DSH runs with, from the `agent-default-model`
/// namespace's value: `(provider, model)` when both are set.
pub fn default_model_of(settings: &Value) -> Option<(String, String)> {
    let (_, value) = settings_namespaces(settings)
        .into_iter()
        .find(|(name, _)| *name == DEFAULT_MODEL_NS)?;
    let provider = value["provider"].as_str()?.to_string();
    let model = value["model"].as_str()?.to_string();
    Some((provider, model))
}

/// The `settings/mutate` params setting the default model. `add` writes the
/// key whether or not it exists yet; the revision is left unset so a
/// last-write wins, matching the store's own merge rule.
pub fn set_default_model_params(provider: &str, model: &str) -> Value {
    json!({
        "ns": DEFAULT_MODEL_NS,
        "ops": [
            { "op": "add", "path": ["provider"], "value": provider },
            { "op": "add", "path": ["model"], "value": model },
        ],
    })
}

/// How a "Test connection" runs, labeled so the user knows what it can cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestMode {
    /// Store reads only (`credentials/describe` + `settings/describe`): no
    /// request leaves the device, so nothing can be billed.
    Offline,
    /// `llm/discoverModels`: a live provider API call with the saved key,
    /// which can be metered or billed.
    Live,
}

impl TestMode {
    pub fn label(self) -> &'static str {
        match self {
            TestMode::Offline => "Test connection (offline)",
            TestMode::Live => "Test connection (live — billable)",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            TestMode::Offline => {
                "Checks the saved credential and settings in Zeron's store only. \
                 It never contacts the provider, so it cannot bill."
            }
            TestMode::Live => {
                "Calls the provider's model-list API with your saved key. \
                 A real request: it can be metered or billed."
            }
        }
    }

    /// The RPC calls one test of this mode makes, in order. `offline` reads
    /// the store; `live` is the single provider call.
    pub fn calls(self, credential_ref: Option<&str>, settings_ns: &str) -> Vec<(&'static str, Value)> {
        use zeron_rpc::methods;
        match self {
            TestMode::Offline => {
                let mut calls = Vec::new();
                if let Some(ref_) = credential_ref {
                    calls.push((methods::DSH_CREDENTIALS, describe_credentials_params(&[ref_])));
                }
                calls.push((methods::DSH_SETTINGS, json!({})));
                let _ = settings_ns;
                calls
            }
            TestMode::Live => vec![(methods::DSH_DISCOVER_MODELS, discover_models_params(settings_ns))],
        }
    }
}

/// What an offline test concluded, phrased for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineReport {
    pub key_saved: Option<bool>,
    pub namespaces: usize,
}

pub fn offline_report(credential: Option<&Value>, key_ref: Option<&str>, settings: &Value) -> OfflineReport {
    OfflineReport {
        key_saved: match (credential, key_ref) {
            (Some(c), Some(r)) => Some(credential_configured(c, r)),
            _ => None,
        },
        namespaces: settings_namespaces(settings).len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_parses_routes_with_their_refs_and_auth() {
        let catalog = parse_provider_catalog(&json!([
            { "provider": "deepseek-official", "displayName": "DeepSeek", "settingsNs": "llm-deepseek", "apiKeyEnv": "DEEPSEEK_API_KEY", "account": true },
            { "provider": "opencode-go", "displayName": "OpenCode Go", "settingsNs": "llm-pi-ai", "apiKeyEnv": "OPENCODE_API_KEY" },
            { "provider": "chatgpt-subscription", "settingsNs": "llm-pi-ai", "auth": "chatgpt" },
            { "provider": "no-ns" },
        ]));
        assert_eq!(catalog.len(), 3, "an entry without settingsNs is skipped: {catalog:?}");
        let deepseek = &catalog[0];
        assert_eq!(deepseek.id, "deepseek-official");
        assert_eq!(deepseek.display_name, "DeepSeek");
        assert_eq!(deepseek.credential_ref(), Some("DEEPSEEK_API_KEY"));
        assert!(deepseek.account && !deepseek.chatgpt);
        let opencode = &catalog[1];
        assert_eq!(opencode.credential_ref(), Some("OPENCODE_API_KEY"));
        assert!(!opencode.account && !opencode.chatgpt);
        let chatgpt = &catalog[2];
        assert!(chatgpt.chatgpt && !chatgpt.account);
        assert_eq!(chatgpt.credential_ref(), None);
        assert_eq!(chatgpt.display_name, "chatgpt-subscription", "no displayName falls back to the id");
    }

    #[test]
    fn credentials_describe_reads_configured_per_ref() {
        let described = json!({ "OPENCODE_API_KEY": { "configured": true }, "DEEPSEEK_API_KEY": { "configured": false } });
        assert!(credential_configured(&described, "OPENCODE_API_KEY"));
        assert!(!credential_configured(&described, "DEEPSEEK_API_KEY"));
        assert!(!credential_configured(&described, "MISSING"));
        assert_eq!(
            describe_credentials_params(&["A", "B"]),
            json!({ "refs": ["A", "B"] })
        );
        assert_eq!(
            set_credential_params("DEEPSEEK_API_KEY", "sk-x"),
            json!({ "ref": "DEEPSEEK_API_KEY", "value": "sk-x" })
        );
        assert_eq!(unset_credential_params("DEEPSEEK_API_KEY"), json!({ "ref": "DEEPSEEK_API_KEY" }));
    }

    #[test]
    fn account_and_chatgpt_states_parse_defensively() {
        let account = parse_account_state(&json!({ "status": "signed-in", "signedIn": true }));
        assert!(account.signed_in && account.status == "signed-in");
        assert!(!parse_account_state(&json!({ "status": "signed-out" })).signed_in);
        let start = parse_sign_in_start(&json!({ "url": "http://127.0.0.1:1455/cb", "attemptId": "a1" }));
        assert_eq!(start.url.as_deref(), Some("http://127.0.0.1:1455/cb"));
        assert_eq!(start.attempt_id.as_deref(), Some("a1"));
        assert!(parse_sign_in_start(&json!({})).url.is_none());
        let status = parse_chatgpt_status(&json!({ "authorized": true, "running": false, "url": null }));
        assert!(status.authorized && !status.running && status.url.is_none());
        assert_eq!(
            parse_chatgpt_start(&json!({ "url": "https://auth.openai.com/api/accounts/authorize?x=1" })).as_deref(),
            Some("https://auth.openai.com/api/accounts/authorize?x=1")
        );
    }

    #[test]
    fn models_parse_from_a_bare_array_or_a_wrapped_one() {
        let bare = parse_models(&json!([{ "id": "deepseek-flash", "name": "Flash" }, { "id": "m2" }]));
        assert_eq!(bare[0], ModelChoice { id: "deepseek-flash".into(), label: "Flash".into() });
        assert_eq!(bare[1].label, "m2", "no name falls back to the id");
        let wrapped = parse_models(&json!({ "models": [{ "id": "a", "displayName": "A" }] }));
        assert_eq!(wrapped, vec![ModelChoice { id: "a".into(), label: "A".into() }]);
        assert!(parse_models(&json!({})).is_empty());
    }

    #[test]
    fn settings_namespaces_and_models_within_one_read_offline() {
        let settings = json!({ "namespaces": [
            { "ns": "llm-pi-ai", "value": { "providers": {}, "models": [{ "id": "gpt-5", "name": "GPT-5" }] } },
            { "ns": "agent-default-model", "value": { "provider": "opencode-go", "model": "gpt-5" } },
        ] });
        let namespaces = settings_namespaces(&settings);
        assert_eq!(namespaces.len(), 2);
        let models = models_in_ns(&settings, "llm-pi-ai");
        assert_eq!(models, vec![ModelChoice { id: "gpt-5".into(), label: "GPT-5".into() }]);
        assert!(models_in_ns(&settings, "missing").is_empty());
        assert_eq!(
            default_model_of(&settings).as_ref(),
            Some(&("opencode-go".to_string(), "gpt-5".to_string()))
        );
        assert!(default_model_of(&json!({ "namespaces": [] })).is_none());
    }

    #[test]
    fn set_default_model_mutates_the_default_ns_last_write_wins() {
        let params = set_default_model_params("opencode-go", "gpt-5");
        assert_eq!(params["ns"], "agent-default-model");
        assert!(params["expectedRevision"].is_null(), "no optimistic revision: the store merges last-write-wins");
        assert_eq!(
            params["ops"],
            json!([
                { "op": "add", "path": ["provider"], "value": "opencode-go" },
                { "op": "add", "path": ["model"], "value": "gpt-5" },
            ])
        );
        assert_eq!(
            discover_models_params("llm-pi-ai"),
            json!({ "settingsNs": "llm-pi-ai" })
        );
    }

    #[test]
    fn test_modes_are_labeled_with_what_they_can_cost() {
        assert!(TestMode::Offline.label().contains("offline"));
        assert!(TestMode::Offline.detail().contains("cannot bill"));
        assert!(TestMode::Live.label().contains("billable"));
        assert!(TestMode::Live.detail().contains("metered or billed"));
        let offline = TestMode::Offline.calls(Some("DEEPSEEK_API_KEY"), "llm-deepseek");
        assert_eq!(offline.len(), 2, "offline reads credentials then settings, nothing else");
        assert_eq!(offline[0].0, zeron_rpc::methods::DSH_CREDENTIALS);
        assert_eq!(offline[1].0, zeron_rpc::methods::DSH_SETTINGS);
        let live = TestMode::Live.calls(Some("DEEPSEEK_API_KEY"), "llm-pi-ai");
        assert_eq!(live.len(), 1, "live is exactly the one provider call");
        assert_eq!(live[0].0, zeron_rpc::methods::DSH_DISCOVER_MODELS);
        assert_eq!(live[0].1, json!({ "settingsNs": "llm-pi-ai" }));
    }

    #[test]
    fn the_offline_report_says_whether_the_key_is_saved() {
        let settings = json!({ "namespaces": [{ "ns": "agent-default-model", "value": {} }] });
        let report = offline_report(
            Some(&json!({ "DEEPSEEK_API_KEY": { "configured": true } })),
            Some("DEEPSEEK_API_KEY"),
            &settings,
        );
        assert_eq!(report.key_saved, Some(true));
        assert_eq!(report.namespaces, 1);
        assert_eq!(offline_report(None, Some("DEEPSEEK_API_KEY"), &settings).key_saved, None);
    }
}
