//! Typed shapes for DSH's gateway surface, parsed for the app's settings UI.
//!
//! [`crate::dsh_gateway`] forwards the calls; this module reads and builds
//! their argument and answer shapes, so the UI never guesses at a wire field.
//! Every parser is defensive: a field DSH renames degrades to `None`, never
//! to a wrong answer.

use serde_json::{json, Value};
use zeron_proto::ReasoningLevel;

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
    /// The endpoint the catalog answers for this route (`defaultBaseURL`),
    /// used when the stored profile names no `baseURL` of its own.
    pub default_base_url: Option<String>,
    /// Whether the route signs in through DSH's own account flow.
    pub account: bool,
    /// Whether the route is one someone declared by hand rather than one pi-ai
    /// ships (`declared` in the catalog answer), which is the only kind Zeron
    /// may remove: a catalog route's path always exists.
    pub declared: bool,
    /// Whether the route is the ChatGPT subscription, served by Zeron's own
    /// plugin on the same gateway (`zeron-chatgpt/*`).
    pub chatgpt: bool,
    /// Whether the route has an interactive OAuth flow on this gateway.
    pub oauth: bool,
}

impl GatewayProvider {
    /// The credential ref to describe/set/unset for this route.
    pub fn credential_ref(&self) -> Option<&str> {
        self.api_key_env.as_deref()
    }
}

/// The models a provider's own API serves to a route's saved key.
#[async_trait::async_trait]
pub trait AccountModels: Send + Sync {
    /// The account's models for a route, or why they could not be read
    /// (no resolvable key, refused, unreachable, a body that is not a list).
    async fn list(&self, route: &GatewayProvider) -> Result<Vec<ModelChoice>, String>;
}

/// An account read that never answers: every route falls back to the SDK's
/// discovered catalog. For assemblies and tests without a credential store
/// to read; the app's picker wires [`GatewayAccountModels`].
pub struct NoAccountModels;

#[async_trait::async_trait]
impl AccountModels for NoAccountModels {
    async fn list(&self, route: &GatewayProvider) -> Result<Vec<ModelChoice>, String> {
        Err(format!("{}: the account model read is not wired", route.id))
    }
}

/// Where a route's picker list came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// The provider's own API answered for the route's saved key.
    Account,
    /// The route keeps discovery — it never reads an account (a native
    /// route, the ChatGPT subscription, an oauth route with no stored ref) —
    /// so its list is not a catalog fallback and carries no label.
    Discovery,
    /// The account read was attempted and did not answer (a refusal, a
    /// transport failure, or an empty list).
    CatalogFallback(String),
}

/// The models a configured route's picker shows: the account's own list when
/// the provider's API answers for its key, the SDK's discovered catalog when
/// the account read cannot answer. The catalog stands in with the refusal it
/// answered for carried verbatim — an account that answers with nothing is a
/// fallback too, never a silent `Account`.
pub fn account_or_catalog(
    account: Result<Vec<ModelChoice>, String>,
    catalog: Vec<ModelChoice>,
) -> (Vec<ModelChoice>, ModelSource) {
    match account {
        Ok(models) if !models.is_empty() => (models, ModelSource::Account),
        Ok(_) => (catalog, ModelSource::CatalogFallback("empty".to_string())),
        Err(reason) => (catalog, ModelSource::CatalogFallback(reason)),
    }
}

/// The credential file the gateway writes every save into
/// (`credentials` in the shim's `dsh web` profile, mode 0600).
const CREDENTIALS_FILE: &str = ".credentials.yaml";

/// A saved key, read back out of the gateway's credential store on every
/// ask. The store's `refs:` block maps each env-shaped reference to its
/// value; `records:` above it holds the store's own session material, none of
/// which is a ref. `Option`, not `Result`: an unreadable file, a missing ref
/// and an empty value are the same "no key" to the caller, and the value
/// never travels inside an error string.
pub fn stored_key(home: &std::path::Path, reference: &str) -> Option<String> {
    let body = std::fs::read_to_string(home.join(CREDENTIALS_FILE)).ok()?;
    let mut in_refs = false;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if in_refs {
            if !line.starts_with([' ', '\t']) {
                break;
            }
            if let Some((name, value)) = line.trim().split_once(':')
                && name.trim() == reference
            {
                let value = value.trim().trim_matches(['"', '\'']);
                return (!value.is_empty()).then(|| value.to_string());
            }
        } else {
            in_refs = line.trim_end() == "refs:";
        }
    }
    None
}

/// The models endpoint under a route's base URL. Catalog and declared
/// endpoints already carry the API's version segment (`.../v1`, `.../v4`,
/// `.../v1beta`), so only a bare host like `https://api.mistral.ai` takes the
/// OpenAI `/v1` prefix; appending it twice would 404.
fn models_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    let versioned = base.rsplit('/').next().is_some_and(|segment| {
        segment.len() > 1 && segment.starts_with('v') && segment[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
    });
    if versioned {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

/// The HTTP client the account reads share: one build, the picker's ~10 s
/// ceiling on every call.
static ACCOUNT_HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default()
});

/// The live account list. Nothing is cached in process memory: the key is
/// read back out of `<home>/.credentials.yaml` and the endpoint out of the
/// gateway's settings on every call, so a picker opened days after a save —
/// a cold start — still sees the account's own models.
pub struct GatewayAccountModels {
    gateway: std::sync::Arc<zeron_harness::deepseek::gateway::Gateway>,
    home: std::path::PathBuf,
    /// The `settings/describe` answer, fetched once per picker pass: a
    /// declared route's `providers.<route>.baseURL` override lives there.
    settings: tokio::sync::OnceCell<Option<Value>>,
}

impl GatewayAccountModels {
    pub fn new(
        gateway: std::sync::Arc<zeron_harness::deepseek::gateway::Gateway>,
        home: std::path::PathBuf,
    ) -> Self {
        Self {
            gateway,
            home,
            settings: tokio::sync::OnceCell::new(),
        }
    }

    async fn settings(&self) -> Option<&Value> {
        self.settings
            .get_or_init(|| async {
                self.gateway.call("settings/describe", json!({})).await.ok()
            })
            .await
            .as_ref()
    }
}

#[async_trait::async_trait]
impl AccountModels for GatewayAccountModels {
    async fn list(&self, route: &GatewayProvider) -> Result<Vec<ModelChoice>, String> {
        let reference = route.credential_ref().ok_or_else(|| "no key".to_string())?;
        let key = match stored_key(&self.home, reference) {
            Some(key) => key,
            None => self
                .gateway
                .dotenv_key(reference)
                .await
                .ok_or_else(|| "no key".to_string())?,
        };
        // A declared route's profile baseURL wins; a catalog route answers
        // on its `defaultBaseURL`.
        let base = self
            .settings()
            .await
            .and_then(|store| {
                settings_namespaces(store)
                    .into_iter()
                    .find(|(ns, _)| *ns == PI_AI_NS)
                    .and_then(|(_, value)| {
                        value["providers"][route.id.as_str()]["baseURL"]
                            .as_str()
                            .map(str::to_string)
                    })
            })
            .or_else(|| route.default_base_url.clone())
            .ok_or_else(|| "no endpoint".to_string())?;
        let response = ACCOUNT_HTTP
            .get(models_url(&base))
            .bearer_auth(key)
            .send()
            .await
            .map_err(|_| "offline".to_string())?;
        if !response.status().is_success() {
            return Err(response.status().as_u16().to_string());
        }
        let body: Value = response
            .json()
            .await
            .map_err(|_| "unreadable".to_string())?;
        Ok(account_chat_models(&body))
    }
}

/// The account's chat-capable models: id/label parsing is `parse_models`',
/// then each row's `capabilities.completion_chat` flag decides — `false`
/// drops. `true`, absent or a non-boolean value keeps the row, including
/// rows the discovered catalog has never heard of: the flag drops what the
/// provider marks non-chat and nothing else, because the picker must not
/// narrow the account's list to the shipped catalog.
fn account_chat_models(value: &Value) -> Vec<ModelChoice> {
    let rows = value.as_array().or_else(|| {
        ["models", "items", "data"]
            .iter()
            .find_map(|key| value[key].as_array())
    });
    let chat: std::collections::HashMap<&str, Option<bool>> = rows
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some((
                row["id"].as_str()?,
                row["capabilities"]["completion_chat"].as_bool(),
            ))
        })
        .collect();
    parse_models(value)
        .into_iter()
        .filter(|choice| !matches!(chat.get(choice.id.as_str()), Some(Some(false))))
        .collect()
}

/// Whether a configured route's picker reads the account's own models — the
/// provider's API answering for the route's saved key — rather than the
/// discovered catalog.
fn reads_the_account(route: &GatewayProvider, credential_ready: bool) -> bool {
    route.settings_ns == PI_AI_NS
        && credential_ready
        && route.credential_ref().is_some()
        // The ChatGPT subscription has no `nativeAuth.env` ref — the check
        // above already excludes it — and must keep discovery; the explicit
        // guard stays so no future shape of it takes the account path.
        && !route.chatgpt
}

/// What one route's picker list is: its discovered catalog, or the account's
/// own models with the catalog as a labelled fallback.
fn route_choices(
    route: &GatewayProvider,
    credential_ready: bool,
    account: Result<Vec<ModelChoice>, String>,
    discovered: Vec<ModelChoice>,
) -> (Vec<ModelChoice>, ModelSource) {
    if reads_the_account(route, credential_ready) {
        return account_or_catalog(account, discovered);
    }
    // The account read is not this route's to make — discovery is its
    // picker list outright, not a fallback — so `account`, whatever the
    // caller passes for a read that never happened, is set aside.
    (discovered, ModelSource::Discovery)
}

/// The hint a source's rows carry: `None` for the account's own list and for
/// a route that keeps discovery; `Shipped catalog — {reason}` only when the
/// account read was attempted and did not answer.
fn description_of(source: &ModelSource) -> Option<String> {
    match source {
        ModelSource::Account | ModelSource::Discovery => None,
        ModelSource::CatalogFallback(reason) => Some(format!("Shipped catalog — {reason}")),
    }
}

/// One route's rows, with the hint each carries: `None` for the account's own
/// list and for a route that never read one; `Shipped catalog — {reason}` only
/// when the account read was attempted and did not answer.
pub(crate) fn route_rows(
    route: &GatewayProvider,
    credential_ready: bool,
    account: Result<Vec<ModelChoice>, String>,
    discovered: Vec<ModelChoice>,
) -> Vec<(ModelChoice, Option<String>)> {
    let (choices, source) = route_choices(route, credential_ready, account, discovered);
    let description = description_of(&source);
    choices
        .into_iter()
        .map(|choice| (choice, description.clone()))
        .collect()
}

/// The models the gateway's configured routes offer, for the model picker.
///
/// A route counts as configured when its `apiKeyEnv` reference is stored, or
/// when it is the ChatGPT subscription and the account is signed in. Best
/// effort throughout: discovery is a network call per route, so a route that
/// cannot answer contributes nothing and the caller keeps its own list.
pub async fn route_models(
    gateway: &zeron_harness::deepseek::gateway::Gateway,
    chatgpt_signed_in: &std::collections::HashSet<String>,
    oauth_signed_in: &std::collections::HashSet<String>,
    account: &dyn AccountModels,
) -> Vec<zeron_proto::Model> {
    use zeron_proto::{Model, ModelProvider};

    let Ok(catalog) = gateway
        .call("llm/listConfigurableProviders", serde_json::json!({}))
        .await
    else {
        return Vec::new();
    };
    let routes = parse_provider_catalog(&catalog);
    let refs: Vec<&str> = routes.iter().filter_map(GatewayProvider::credential_ref).collect();
    let describe = if refs.is_empty() {
        None
    } else {
        gateway
            .call("credentials/describe", describe_credentials_params(&refs))
            .await
            .ok()
    };
    let mut models = Vec::new();
    let mut settings = gateway.call("settings/describe", json!({})).await.ok();
    // `llm/discoverModels` strips capability data: the adapter emits id,
    // name, contextWindow, maxTokens and inputModalities only, and the host
    // re-projects those same fields (`discoverModels` in dsh-llm-pi-ai and
    // `LlmRuntime.discoverModels` in dsh-llm). Reasoning support is instead
    // declared where the adapter itself resolves it, so the levels offered
    // here are mirrored from those declarations rather than guessed — an
    // effort the run would refuse (UNSUPPORTED_REASONING_EFFORT) is never
    // shown:
    //
    // * pi-ai routes (`llm-pi-ai` profiles): the installed catalog at
    //   `@earendil-works/pi-ai` `providers/data/<route>.json` carries each
    //   chat model's `reasoning` flag and `thinkingLevelMap` wire spellings,
    //   overlaid by the stored profile's `models[].reasoningEfforts` and
    //   `modelOverrides` (`resolveModelReasoning`/`getSupportedThinkingLevels`).
    // * llm-deepseek routes: every model offers the adapter's fixed effort
    //   list (off/low/high/max) unless the route's `thinking` switch is
    //   `disabled`, which reduces the offer to `off` alone (`modelInfo` in
    //   dsh-llm-deepseek).
    //
    // The harness only ever sends low, high and max (`EFFORTS` in
    // `deepseek::configuration`), so supported levels outside those — pi-ai's
    // `minimal`/`medium`/`xhigh`, and every route's `off` — are filtered out
    // of what the picker offers.
    let catalog_dir = zeron_harness::deepseek::DeepSeekHarness::new()
        .sdk_root()
        .join("node_modules/@earendil-works/pi-ai/dist/providers/data");
    let mut catalogs: std::collections::HashMap<String, std::collections::HashMap<String, Vec<ReasoningLevel>>> =
        std::collections::HashMap::new();
    for route in &routes {
        let credential_ready = route
            .credential_ref()
            .is_some_and(|key| describe.as_ref().is_some_and(|d| credential_configured(d, key)));
        let ready = credential_ready
            || (route.chatgpt && chatgpt_signed_in.contains(&route.id))
            || (route.oauth && oauth_signed_in.contains(&route.id));
        if !ready {
            continue;
        }
        // Discovery can list a catalog model before the pi-ai adapter has a
        // configured profile for its route. A signed-in route needs that
        // profile registered before the runtime can dispatch requests to it.
        if route.settings_ns == PI_AI_NS
            && (route.chatgpt || route.oauth)
            && settings.as_ref().is_none_or(|store| !provider_profile_exists(store, &route.id))
        {
            if gateway
                .call(
                    "settings/mutate",
                    json!({ "ns": PI_AI_NS, "ops": [{ "op": "set", "path": ["providers", route.id], "value": {} }] }),
                )
                .await
                .is_err()
            {
                continue;
            }
            settings = gateway.call("settings/describe", json!({})).await.ok();
        }
        let found = gateway
            .call(
                "llm/discoverModels",
                serde_json::json!({
                    "settingsNs": route.settings_ns,
                    "request": { "provider": route.id },
                }),
            )
            .await
            .unwrap_or(Value::Null);
        let discovered = parse_models(&found);
        // A pi-ai route answering on its own saved key lists the account's
        // models; the discovered catalog stands in only when that read
        // cannot, labeled with why it is the catalog. The stored credential
        // ref gates the read, not the oauth flag: a route can carry both a
        // browser sign-in and a saved apiKeyEnv, and the Mistral route is
        // exactly that. DeepSeek and ChatGPT keep the discovered list.
        let read = if reads_the_account(route, credential_ready) {
            account.list(route).await
        } else {
            // The read is never made for a route that keeps discovery:
            // `route_choices` reports `Discovery` and sets this aside.
            Ok(Vec::new())
        };
        for (choice, description) in route_rows(route, credential_ready, read, discovered) {
            models.push(Model {
                id: choice.id.clone(),
                label: choice.label,
                description,
                provider: Some(ModelProvider {
                    id: route.id.clone(),
                    name: route.display_name.clone(),
                }),
                reasoning_levels: route_reasoning_levels(
                    route,
                    &choice.id,
                    settings.as_ref(),
                    &catalog_dir,
                    &mut catalogs,
                )
                .await,
                options: Vec::new(),
                default: false,
            });
        }
    }
    models
}

/// The pi-ai thinking levels the harness's run path can send
/// (`deepseek::configuration` maps exactly these, in this order).
const RUNNABLE_EFFORTS: [(&str, ReasoningLevel); 6] = [
    ("minimal", ReasoningLevel::Minimal),
    ("low", ReasoningLevel::Low),
    ("medium", ReasoningLevel::Medium),
    ("high", ReasoningLevel::High),
    ("xhigh", ReasoningLevel::XHigh),
    ("max", ReasoningLevel::Max),
];

/// The levels a declared `reasoningEfforts`/`thinkingLevelMap` dict supports.
/// A member is usable when it names a wire value (a string): a `null` member
/// pins the level to unsupported — except `off`, which is not a
/// `ReasoningLevel` and never reaches the list either way.
fn map_supported_levels(map: &Value) -> Vec<ReasoningLevel> {
    RUNNABLE_EFFORTS
        .into_iter()
        .filter(|(level, _)| map[*level].is_string())
        .map(|(_, level)| level)
        .collect()
}

/// The runnable levels an installed catalog model declares. With no
/// `thinkingLevelMap`, pi-ai's base levels (off/minimal/low/medium/high) are
/// all supported and only `xhigh`/`max` are withheld
/// (`getSupportedThinkingLevels`); declared maps decide explicitly.
fn catalog_model_levels(entry: &Value) -> Vec<ReasoningLevel> {
    if entry["reasoning"].as_bool() != Some(true) {
        return Vec::new();
    }
    if entry["thinkingLevelMap"].is_object() {
        return map_supported_levels(&entry["thinkingLevelMap"]);
    }
    // No map: the base levels (`off/minimal/low/medium/high` — `off` is not
    // a `ReasoningLevel`), never `xhigh` or `max`.
    RUNNABLE_EFFORTS
        .into_iter()
        .map(|(_, level)| level)
        .filter(|level| !matches!(level, ReasoningLevel::Max | ReasoningLevel::XHigh))
        .collect()
}

/// Parse one installed catalog file (`providers/data/<route>.json`) into
/// `model id -> runnable levels`. Chat entries are keyed `<type>:<id>` under
/// each API group; `getBuiltinModels` serves only `type: "chat"`, last entry
/// per id winning, which the discovered list then enumerates.
fn parse_catalog_levels(
    file: &Value,
) -> std::collections::HashMap<String, Vec<ReasoningLevel>> {
    let mut levels = std::collections::HashMap::new();
    for group in file.as_object().into_iter().flat_map(|groups| groups.values()) {
        let Some(entries) = group.as_object() else {
            continue;
        };
        for entry in entries.values() {
            if entry["type"].as_str() != Some("chat") {
                continue;
            }
            if let Some(id) = entry["id"].as_str() {
                levels.insert(id.to_string(), catalog_model_levels(entry));
            }
        }
    }
    levels
}

/// The runnable levels a discovered model carries for one route, mirroring
/// the adapter's own resolution. pi-ai routes resolve the stored profile's
/// declaration over the installed catalog, both read the way
/// `resolveModelReasoning` composes them; llm-deepseek routes answer their
/// adapter's fixed set unless `thinking` is disabled. Everything else —
/// including a route whose catalog file is missing — offers nothing, which
/// keeps an effort DSH would refuse out of the picker.
async fn route_reasoning_levels(
    route: &GatewayProvider,
    model: &str,
    settings: Option<&Value>,
    catalog_dir: &std::path::Path,
    catalogs: &mut std::collections::HashMap<String, std::collections::HashMap<String, Vec<ReasoningLevel>>>,
) -> Vec<ReasoningLevel> {
    if route.settings_ns == PI_AI_NS {
        let profile = settings.and_then(|store| {
            settings_namespaces(store)
                .into_iter()
                .find(|(ns, _)| *ns == PI_AI_NS)
                .map(|(_, value)| value["providers"][route.id.as_str()].clone())
        });
        let profile = profile.unwrap_or(Value::Null);
        let models_list = profile["models"].as_array();
        let declared = models_list
            .into_iter()
            .flatten()
            .find(|entry| entry["id"].as_str() == Some(model));
        // A declared models[] entry or — when no models list replaces the
        // catalog — a modelOverrides member decides the level set when it
        // names `reasoningEfforts`; otherwise the installed catalog entry's
        // capability carries through unchanged, exactly as
        // `resolveModelReasoning` composes them.
        let declaration = declared
            .map(|entry| &entry["reasoningEfforts"])
            .filter(|efforts| !efforts.is_null())
            .or_else(|| {
                if models_list.is_some_and(|list| !list.is_empty()) {
                    return None;
                }
                let override_efforts = &profile["modelOverrides"][model]["reasoningEfforts"];
                (!override_efforts.is_null()).then_some(override_efforts)
            });
        if let Some(efforts) = declaration {
            // `false` is a non-reasoning model; a dict declares the offered
            // levels and their wire spellings.
            return efforts
                .as_object()
                .map(|_| map_supported_levels(efforts))
                .unwrap_or_default();
        }
        if !catalogs.contains_key(route.id.as_str()) {
            let path = catalog_dir.join(format!("{}.json", route.id));
            let parsed = tokio::fs::read(&path)
                .await
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .map(|file| parse_catalog_levels(&file))
                .unwrap_or_default();
            catalogs.insert(route.id.clone(), parsed);
        }
        return catalogs[route.id.as_str()]
            .get(model)
            .cloned()
            .unwrap_or_default();
    }
    if route.settings_ns.starts_with("llm-deepseek") {
        let thinking = settings
            .and_then(|store| {
                settings_namespaces(store)
                    .into_iter()
                    .find(|(ns, _)| *ns == route.settings_ns)
                    .map(|(_, value)| {
                        value["thinking"].as_str().unwrap_or("enabled").to_string()
                    })
            })
            .unwrap_or_else(|| "enabled".to_string());
        if thinking == "disabled" {
            return Vec::new();
        }
        // The DeepSeek adapter's own: off, low, high, max.
        return vec![
            ReasoningLevel::Low,
            ReasoningLevel::High,
            ReasoningLevel::Max,
        ];
    }
    Vec::new()
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
            // The pinned DSH describes a route's credential as
            // `nativeAuth: {env: [...], oauth: bool}`; older builds used a
            // bare `apiKeyEnv`/`auth`, which stays as the fallback.
            let api_key_env = entry["nativeAuth"]["env"]
                .as_array()
                .and_then(|env| env.first())
                .and_then(|v| v.as_str())
                .or_else(|| entry["apiKeyEnv"].as_str())
                .map(str::to_string);
            // The ChatGPT subscription only — not every route that *can* sign
            // in through OAuth. A grant on this one must not make the others
            // look configured.
            let chatgpt = id == "openai" || id == "openai-codex";
            // Mistral Vibe's browser sign-in yields a Mistral API key. It is
            // served by Zeron's dedicated flow, while the DSH provider keeps
            // owning the actual Mistral model requests.
            let oauth = (entry["nativeAuth"]["oauth"].as_bool() == Some(true)
                && id != "openai"
                && id != "openai-codex")
                || id == "mistral";
            let account = !chatgpt
                && (entry["account"].as_bool() == Some(true)
                    || entry["auth"].as_str() == Some("account")
                    || id.contains("deepseek"));
            Some(GatewayProvider {
                // Keep the provider's product name in the picker even when
                // the upstream catalog exposes its internal route id.
                display_name: if id == "openai-codex" {
                    "Codex".to_string()
                } else if id == "mistral" {
                    "Mistral Vibe".to_string()
                } else {
                    entry["displayName"].as_str().unwrap_or(&id).to_string()
                },
                id,
                settings_ns,
                api_key_env,
                default_base_url: entry["defaultBaseURL"]
                    .as_str()
                    .map(str::to_string),
                account,
                declared: entry["declared"].as_bool().unwrap_or(false),
                chatgpt,
                oauth,
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

/// Whether pi-ai has the route profile whose presence registers its runtime
/// adapter. Model discovery alone can succeed without this profile.
fn provider_profile_exists(settings: &Value, route: &str) -> bool {
    settings_namespaces(settings)
        .into_iter()
        .find(|(ns, _)| *ns == PI_AI_NS)
        .is_some_and(|(_, value)| value["providers"].get(route).is_some())
}

/// The models a route offers, read offline from the store (no provider call).
/// The route's own profile wins: pi-ai keeps each route at
/// `providers.<route>`, and a hand-declared route's models are its
/// `models` field — the only place they exist. When the profile has no
/// `models` (a catalog route that lists its models live instead), the first
/// array of `{id, ...}` objects under any top-level key of the ns value is
/// the namespace-wide list DSH's card layout stores there.
pub fn models_for_route(settings: &Value, ns: &str, route: &str) -> Vec<ModelChoice> {
    let Some((_, value)) = settings_namespaces(settings).into_iter().find(|(name, _)| *name == ns)
    else {
        return Vec::new();
    };
    if let Some(declared) = value["providers"][route].get("models") {
        return parse_models(declared);
    }
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

/// The `settings/mutate` params setting the default model. `set` writes the
/// key whether or not it exists yet; the revision is left unset so a
/// last-write wins, matching the store's own merge rule.
pub fn set_default_model_params(provider: &str, model: &str) -> Value {
    json!({
        "ns": DEFAULT_MODEL_NS,
        "ops": [
            { "op": "set", "path": ["provider"], "value": provider },
            { "op": "set", "path": ["model"], "value": model },
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

/// The settings namespace pi-ai's adapter mounts, which holds every declared
/// route at `providers.<route>`.
pub const PI_AI_NS: &str = "llm-pi-ai";

/// A route id pi-ai accepts both as a settings key and as the stem of a
/// credential reference: lower-case segments joined by single dashes, starting
/// with a letter, within the runtime's 64-byte route limit
/// (`deepseek::configuration` refuses a longer route before a run). DSH's own
/// custom-provider card enforces the same grammar before it writes, and the
/// credential seam reinforces it: the derived ref is upper-cased, so a
/// digit-leading id would name a ref the store refuses.
pub fn route_id_valid(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    let mut segments = id.split('-');
    let first = segments.next().unwrap_or_default();
    let mut chars = first.chars();
    let head = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    head && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && segments.all(|segment| {
            !segment.is_empty()
                && segment.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// The credential reference a declared route's key is stored under when the
/// declaration does not name one (`deriveKeyRef` in DSH's Models page).
pub fn key_ref_for(route: &str) -> String {    let stem: String = route
        .to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{stem}_API_KEY")
}

/// Whether `value` parses as an absolute http(s) endpoint with a host. The
/// runtime hands `baseURL` to a URL parser, so a bare `https://` or a broken
/// authority must be refused here rather than persisted.
pub fn http_url(value: &str) -> bool {
    reqwest::Url::parse(value)
        .map(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
        .unwrap_or(false)
}

/// Read a typed token capacity, the spelling DSH's Models page accepts: a
/// decimal count with an optional `K`/`M` suffix, so `1M` and `256K` are the
/// numbers they quote. A blank field is `Ok(None)` (inherit); an unreadable one
/// is `Err`, which validation then reports against the row.
pub fn parse_capacity(text: &str) -> Result<Option<u64>, ()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let (digits, scale) = match trimmed.as_bytes().last() {
        Some(b'k' | b'K') => (&trimmed[..trimmed.len() - 1], 1_000.0),
        Some(b'm' | b'M') => (&trimmed[..trimmed.len() - 1], 1_000_000.0),
        _ => (trimmed, 1.0),
    };
    let count: f64 = digits.parse().map_err(|_| ())?;
    if !count.is_finite() || count <= 0.0 {
        return Err(());
    }
    let scaled = count * scale;
    Ok(Some(scaled.round() as u64))
}

/// Whether `value` is an env-shaped credential reference: what `apiKeyEnv`
/// accepts, and what the credential store's `set` accepts.
pub fn key_ref_valid(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `value` is a model id the runtime will run: the grammar
/// `deepseek::configuration` enforces for a non-default route — ASCII
/// alphanumerics and `- _ . : /`, at most 128 bytes. A declaration that wrote
/// a looser id would persist a route every run refuses.
pub fn model_id_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/'))
}

/// One model of a hand-declared route (`models[]` in pi-ai's config).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredModel {
    pub id: String,
    /// Display name; DSH falls back to the id.
    pub name: Option<String>,
    pub context_window: Option<u64>,
    /// The output capability, which pi-ai also sends as the request's token
    /// cap — so this is a real limit, not a label.
    pub max_tokens: Option<u64>,
    /// Whether the model takes images. Vision is declared, never inferred: a
    /// model without it refuses attachments.
    pub vision: bool,
}

impl DeclaredModel {
    fn to_json(&self) -> Value {
        let mut entry = json!({ "id": self.id });
        let object = entry.as_object_mut().expect("object literal");
        if let Some(name) = self.name.as_deref().filter(|name| !name.is_empty()) {
            object.insert("name".into(), json!(name));
        }
        if let Some(context) = self.context_window {
            object.insert("contextWindow".into(), json!(context));
        }
        if let Some(max) = self.max_tokens {
            object.insert("maxTokens".into(), json!(max));
        }
        let input: &[&str] = if self.vision { &["text", "image"] } else { &["text"] };
        object.insert("input".into(), json!(input));
        entry
    }
}

/// The wire-compatibility switches the installed catalog carries for a
/// DeepSeek-compatible `openai-completions` route, and that a hand-declared
/// route loses unless it restates them: without them pi-ai's own
/// `detectCompat` sends `store: false`, rewrites `system` to `developer`,
/// names the token cap `max_completion_tokens`, and asks for reasoning with
/// `reasoning_effort` instead of DeepSeek's `thinking` block. Each is a silent
/// behavior change rather than an error, so the editor states them explicitly.
pub fn deepseek_compat() -> Value {
    json!({
        "supportsStore": false,
        "supportsDeveloperRole": false,
        "supportsStrictMode": true,
        "maxTokensField": "max_tokens",
        "requiresReasoningContentOnAssistantMessages": true,
        "thinkingFormat": "deepseek",
    })
}

/// A route pi-ai's installed catalog does not describe, declared by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredRoute {
    pub id: String,
    pub display_name: Option<String>,
    /// The endpoint prefix up to (not including) the API path.
    pub base_url: String,
    /// The one wire protocol every model on the route speaks.
    pub api: String,
    /// The env-shaped reference the key is read through; `None` leaves the
    /// route on its provider-native authentication (a credential chain, ADC).
    pub api_key_env: Option<String>,
    /// Static headers sent with every request, which a declared route needs
    /// explicitly: the catalog path adds its own.
    pub headers: Vec<(String, String)>,
    pub models: Vec<DeclaredModel>,
    /// Write [`deepseek_compat`] on `openai-completions`.
    pub deepseek_compat: bool,
}

impl DeclaredRoute {
    /// The reference a key would be stored under for this route.
    pub fn key_ref(&self) -> String {
        self.api_key_env.clone().unwrap_or_else(|| key_ref_for(&self.id))
    }

    /// The profile pi-ai reads at `providers.<id>`.
    pub fn profile(&self) -> Value {
        let mut profile = json!({ "api": self.api, "baseURL": self.base_url });
        let object = profile.as_object_mut().expect("object literal");
        if let Some(name) = self.display_name.as_deref().filter(|name| !name.is_empty()) {
            object.insert("displayName".into(), json!(name));
        }
        if let Some(key_ref) = &self.api_key_env {
            object.insert("apiKeyEnv".into(), json!(key_ref));
        }
        if !self.headers.is_empty() {
            let headers: serde_json::Map<String, Value> = self
                .headers
                .iter()
                .map(|(name, value)| (name.clone(), json!(value)))
                .collect();
            object.insert("headers".into(), Value::Object(headers));
        }
        if self.deepseek_compat && self.api == "openai-completions" {
            object.insert("compat".into(), deepseek_compat());
        }
        object.insert(
            "models".into(),
            Value::Array(self.models.iter().map(DeclaredModel::to_json).collect()),
        );
        profile
    }
}

/// Why a declaration was refused, named so the form can point at the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationError {
    /// The id is not route-shaped.
    RouteId(String),
    /// A route of that id already exists (catalog or declared).
    RouteTaken(String),
    BaseUrl(String),
    /// The protocol is not one the adapter serves.
    Api(String),
    /// A declared route cannot inherit a catalog, so it needs a model.
    NoModels,
    ModelId(usize),
    DuplicateModel(usize),
    /// A capacity that is not a whole positive token count.
    Capacity(usize, &'static str),
    KeyRef(String),
    /// A header name or value a request would reject.
    Header(usize),
}

impl DeclarationError {
    /// The sentence the form shows beside the offending field.
    pub fn message(&self) -> String {
        match self {
            Self::RouteId(id) => format!(
                "{id} is not a route id: lower-case segments joined by dashes, starting with a letter, at most 64 bytes (acme-gateway)."
            ),
            Self::RouteTaken(id) => format!("{id} is already a route on this device."),
            Self::BaseUrl(url) => format!("{url} is not an http(s) endpoint."),
            Self::Api(api) => format!("{api} is not a protocol this adapter serves."),
            Self::NoModels => "a declared route needs at least one model: it has no catalog to inherit.".into(),
            Self::ModelId(index) => format!(
                "model {} needs an id the runtime accepts: ASCII letters, digits and - _ . : /, at most 128 bytes.",
                index + 1
            ),
            Self::DuplicateModel(index) => format!("model {} repeats an id another row already uses.", index + 1),
            Self::Capacity(index, field) => {
                format!("model {}: {field} must be a whole number of tokens greater than zero.", index + 1)
            }
            Self::KeyRef(reference) => format!(
                "{reference} is not a credential reference: letters, digits and underscores, not starting with a digit."
            ),
            Self::Header(index) => format!("header {} needs a name without spaces and a value on one line.", index + 1),
        }
    }
}

impl std::fmt::Display for DeclarationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for DeclarationError {}

/// Check a declaration before it is written. `taken` is every route id the
/// device already has, catalog and declared alike.
pub fn validate(route: &DeclaredRoute, taken: &[String]) -> Result<(), DeclarationError> {
    if !route_id_valid(&route.id) {
        return Err(DeclarationError::RouteId(route.id.clone()));
    }
    if taken.iter().any(|id| id == &route.id) {
        return Err(DeclarationError::RouteTaken(route.id.clone()));
    }
    if !http_url(&route.base_url) {
        return Err(DeclarationError::BaseUrl(route.base_url.clone()));
    }
    if !protocols().iter().any(|api| api == &route.api) {
        return Err(DeclarationError::Api(route.api.clone()));
    }
    if let Some(key_ref) = &route.api_key_env
        && !key_ref_valid(key_ref)
    {
        return Err(DeclarationError::KeyRef(key_ref.clone()));
    }
    if let Some(index) = route.headers.iter().position(|(name, value)| {
        name.is_empty()
            || name.chars().any(|c| c.is_whitespace() || c == ':' || c == '\n' || c == '\r')
            || value.contains(['\n', '\r'])
    }) {
        return Err(DeclarationError::Header(index));
    }
    if route.models.is_empty() {
        return Err(DeclarationError::NoModels);
    }
    let mut seen: Vec<&str> = Vec::new();
    for (index, model) in route.models.iter().enumerate() {
        let id = model.id.as_str();
        if !model_id_valid(id) {
            return Err(DeclarationError::ModelId(index));
        }
        if seen.contains(&id) {
            return Err(DeclarationError::DuplicateModel(index));
        }
        seen.push(id);
        if model.context_window == Some(0) {
            return Err(DeclarationError::Capacity(index, "the context window"));
        }
        if model.max_tokens == Some(0) {
            return Err(DeclarationError::Capacity(index, "the output limit"));
        }
    }
    Ok(())
}

/// The wire protocols pi-ai serves, in the order the adapter reports them.
/// DSH reads them out of the namespace's own schema; a declaration validated
/// offline uses this list, which the schema read reproduces.
pub fn protocols() -> &'static [&'static str] {
    &["openai-completions", "openai-responses", "anthropic-messages", "google-generative-ai"]
}

/// The protocols a `settings/describe` answer's `llm-pi-ai` schema declares,
/// read from the union at `providers.<any>/api` the same way DSH's Models page
/// reads them, so the form offers exactly what the adapter accepts. Falls back
/// to [`protocols`] when the schema is not walkable.
pub fn protocol_choices(settings: &Value) -> Vec<String> {
    let declared = settings["namespaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["ns"].as_str() == Some(PI_AI_NS))
        .filter(|entry| entry["schema"].is_object())
        .and_then(|entry| schema_at(&entry["schema"], &["providers", "declared-route", "api"]))
        .filter(|node| node["type"].as_str() == Some("union"))
        .and_then(|node| node["list"].as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["value"].as_str().map(str::to_string))
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    if declared.is_empty() {
        protocols().iter().map(|api| api.to_string()).collect()
    } else {
        declared
    }
}

/// Walk a serialized settings schema (`{uid, refs}`) down a value path, the
/// shape `settings/describe` returns for every namespace.
fn schema_at<'a>(schema: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut node = schema_ref(schema, schema.get("uid")?);
    for step in path {
        let next = match node["type"].as_str()? {
            "object" => schema_ref(schema, node["dict"].get(*step)?),
            "dict" => schema_ref(schema, node.get("inner")?),
            _ => return None,
        };
        node = next;
    }
    Some(node)
}

/// Resolve a schema reference: a number is a `refs` key, an object is inline.
fn schema_ref<'a>(schema: &'a Value, node: &'a Value) -> &'a Value {
    match node.as_u64() {
        Some(uid) => schema["refs"].get(uid.to_string()).unwrap_or(&Value::Null),
        None => node,
    }
}

/// The revision of one namespace's user section, for a write that refuses to
/// overwrite a route another window declared meanwhile.
pub fn namespace_revision(settings: &Value, ns: &str) -> Option<u64> {
    settings["namespaces"]
        .as_array()?
        .iter()
        .find(|entry| entry["ns"].as_str() == Some(ns))?
        .get("revision")?
        .as_u64()
}

/// The `settings/mutate` params declaring `route` at `providers.<id>`.
///
/// One path-addressed `set`, the write DSH's own Models page makes: the whole
/// profile at that path, never `settings/replace`, which would drop the secrets
/// the describe read redacts.
pub fn declare_route_params(
    route: &DeclaredRoute,
    taken: &[String],
    revision: Option<u64>,
) -> Result<Value, DeclarationError> {
    validate(route, taken)?;
    let mut params = json!({
        "ns": PI_AI_NS,
        "ops": [
            { "op": "set", "path": ["providers", route.id], "value": route.profile() },
        ],
    });
    if let Some(revision) = revision {
        params["expectedRevision"] = json!(revision);
    }
    Ok(params)
}

/// The `settings/mutate` params removing a declared route. Removal is the only
/// off switch pi-ai's profile has: a route has no `enabled` field, and deleting
/// its settings path is what DSH's own Models page does.
pub fn remove_route_params(route: &str) -> Value {
    json!({
        "ns": PI_AI_NS,
        "ops": [{ "op": "unset", "path": ["providers", route] }],
    })
}

/// Every route id a device has in `llm-pi-ai`, declared or not, so a new
/// declaration cannot shadow one.
pub fn routes_in_ns(settings: &Value, ns: &str) -> Vec<String> {
    settings["namespaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["ns"].as_str() == Some(ns))
        .and_then(|entry| entry["value"]["providers"].as_object())
        .map(|providers| providers.keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_parses_routes_with_their_refs_and_auth() {
        let catalog = parse_provider_catalog(&json!([
            { "provider": "deepseek-official", "displayName": "DeepSeek", "settingsNs": "llm-deepseek", "apiKeyEnv": "DEEPSEEK_API_KEY", "account": true },
            { "provider": "opencode-go", "displayName": "OpenCode Go", "settingsNs": "llm-pi-ai", "apiKeyEnv": "OPENCODE_API_KEY" },
            { "provider": "openai", "settingsNs": "llm-pi-ai", "auth": "chatgpt" },
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
        assert_eq!(chatgpt.display_name, "openai", "no displayName falls back to the id");
        assert!(!chatgpt.declared, "only a hand-declared route is removable");
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
            { "ns": "llm-pi-ai", "value": {
                "providers": {
                    "acme-gateway": {
                        "api": "openai-completions",
                        "models": [{ "id": "acme-flash", "name": "Acme Flash" }, { "id": "acme-mini" }],
                    },
                    "opencode-go": { "api": "openai-completions" },
                },
                "models": [{ "id": "gpt-5", "name": "GPT-5" }],
            } },
            { "ns": "agent-default-model", "value": { "provider": "opencode-go", "model": "gpt-5" } },
        ] });
        let namespaces = settings_namespaces(&settings);
        assert_eq!(namespaces.len(), 2);
        let declared = models_for_route(&settings, "llm-pi-ai", "acme-gateway");
        assert_eq!(
            declared,
            vec![
                ModelChoice { id: "acme-flash".into(), label: "Acme Flash".into() },
                ModelChoice { id: "acme-mini".into(), label: "acme-mini".into() },
            ],
            "a declared route's models live at providers.<route>.models"
        );
        let catalog = models_for_route(&settings, "llm-pi-ai", "opencode-go");
        assert_eq!(
            catalog,
            vec![ModelChoice { id: "gpt-5".into(), label: "GPT-5".into() }],
            "a route without its own models reads the namespace-wide list"
        );
        assert!(models_for_route(&settings, "missing", "acme-gateway").is_empty());
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
                { "op": "set", "path": ["provider"], "value": "opencode-go" },
                { "op": "set", "path": ["model"], "value": "gpt-5" },
            ]),
            "the store's own op vocabulary is set/unset: DSH's Models page writes `set`"
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

    fn declared_route() -> DeclaredRoute {
        DeclaredRoute {
            id: "acme-gateway".into(),
            display_name: Some("Acme Gateway".into()),
            base_url: "https://gateway.acme.test/openai/v1".into(),
            api: "openai-completions".into(),
            api_key_env: Some("ACME_GATEWAY_API_KEY".into()),
            headers: vec![("x-acme-tenant".into(), "zeron".into())],
            models: vec![
                DeclaredModel {
                    id: "acme-flash".into(),
                    name: Some("Acme Flash".into()),
                    context_window: Some(1_000_000),
                    max_tokens: Some(64_000),
                    vision: true,
                },
                DeclaredModel { id: "acme-mini".into(), ..Default::default() },
            ],
            deepseek_compat: true,
        }
    }

    #[test]
    fn route_ids_and_key_refs_follow_the_adapters_grammar() {
        for valid in ["acme-gateway", "a", "route-2", "z9-x"] {
            assert!(route_id_valid(valid), "{valid} is route-shaped");
        }
        let over_limit = format!("{}-x", "a".repeat(64));
        for invalid in ["", "Acme", "acme_gateway", "-acme", "acme-", "acme--gateway", "9acme", "acme gateway", over_limit.as_str()] {
            assert!(!route_id_valid(invalid), "{invalid} is not route-shaped");
        }
        assert_eq!(key_ref_for("acme-gateway"), "ACME_GATEWAY_API_KEY");
        assert_eq!(key_ref_for("a1"), "A1_API_KEY");
        assert!(key_ref_valid("ACME_GATEWAY_API_KEY"));
        assert!(!key_ref_valid("llm-pi-ai/opencode-go"), "a record key is not an env ref");
        assert!(!key_ref_valid("9ACME"));
        for valid in ["gpt-5", "deepseek.v4_flash", "vendor/model:free", "q3_8b.gguf"] {
            assert!(model_id_valid(valid), "{valid} is a model id the runtime runs");
        }
        let too_long = "m".repeat(129);
        for invalid in ["", "acme flash", " acme-flash", "m1 ", "m²", "a,b", too_long.as_str()] {
            assert!(!model_id_valid(invalid), "{invalid} is not a model id the runtime runs");
        }
        assert!(model_id_valid(&"m".repeat(128)), "128 bytes is the runtime's limit");
        assert!(http_url("http://127.0.0.1:41999/v1") && http_url("https://x.test"));
        // WHATWG special schemes skip extra slashes before the authority, so
        // `http:///v1` is `http://v1/` — the parser the runtime uses accepts
        // it. A bare scheme and a broken authority are what cannot parse.
        for invalid in ["https://", "https://[invalid", "ftp://x.test", "gateway.test/v1"] {
            assert!(!http_url(invalid), "{invalid} is not a parsed http(s) endpoint");
        }
    }

    #[test]
    fn a_declaration_writes_one_path_addressed_set_of_the_whole_profile() {
        let params = declare_route_params(&declared_route(), &["opencode-go".into()], Some(7)).unwrap();
        assert_eq!(params["ns"], "llm-pi-ai");
        assert_eq!(params["expectedRevision"], 7);
        assert_eq!(params["ops"].as_array().map(Vec::len), Some(1), "one op, not a rewrite of the namespace");
        assert_eq!(params["ops"][0]["op"], "set");
        assert_eq!(params["ops"][0]["path"], json!(["providers", "acme-gateway"]));
        let profile = &params["ops"][0]["value"];
        assert_eq!(profile["api"], "openai-completions");
        assert_eq!(profile["baseURL"], "https://gateway.acme.test/openai/v1");
        assert_eq!(profile["displayName"], "Acme Gateway");
        assert_eq!(profile["apiKeyEnv"], "ACME_GATEWAY_API_KEY");
        assert_eq!(profile["headers"], json!({ "x-acme-tenant": "zeron" }));
        assert_eq!(
            profile["models"][0],
            json!({
                "id": "acme-flash",
                "name": "Acme Flash",
                "contextWindow": 1_000_000,
                "maxTokens": 64_000,
                "input": ["text", "image"],
            })
        );
        assert_eq!(
            profile["models"][1],
            json!({ "id": "acme-mini", "input": ["text"] }),
            "unset capacities inherit, and vision stays text-only unless declared"
        );
        assert_eq!(
            profile["compat"],
            json!({
                "supportsStore": false,
                "supportsDeveloperRole": false,
                "supportsStrictMode": true,
                "maxTokensField": "max_tokens",
                "requiresReasoningContentOnAssistantMessages": true,
                "thinkingFormat": "deepseek",
            }),
            "a declared route restates the catalog's wire switches"
        );
    }

    #[test]
    fn a_declaration_without_a_revision_leaves_the_field_out() {
        let params = declare_route_params(&declared_route(), &[], None).unwrap();
        assert!(params.get("expectedRevision").is_none(), "an absent revision is absent, not null");
        let mut route = declared_route();
        route.api = "anthropic-messages".into();
        let params = declare_route_params(&route, &[], None).unwrap();
        assert!(
            params["ops"][0]["value"].get("compat").is_none(),
            "the DeepSeek switches belong to openai-completions only; another protocol would refuse them"
        );
    }

    #[test]
    fn validation_names_the_field_it_refuses() {
        let taken = vec!["opencode-go".to_string()];
        let mut route = declared_route();
        route.id = "opencode-go".into();
        assert_eq!(validate(&route, &taken), Err(DeclarationError::RouteTaken("opencode-go".into())));
        assert!(validate(&route, &taken).unwrap_err().message().contains("already a route"));

        route.id = "Acme".into();
        assert!(matches!(validate(&route, &[]), Err(DeclarationError::RouteId(_))));

        let mut route = declared_route();
        route.base_url = "gateway.acme.test".into();
        assert!(matches!(validate(&route, &[]), Err(DeclarationError::BaseUrl(_))));

        route.base_url = "https://gateway.acme.test/v1".into();
        route.api = "carrier-pigeon".into();
        assert!(matches!(validate(&route, &[]), Err(DeclarationError::Api(_))));

        let mut route = declared_route();
        route.models.clear();
        assert_eq!(validate(&route, &[]), Err(DeclarationError::NoModels));

        route.models = vec![DeclaredModel { id: " acme-flash".into(), ..Default::default() }];
        assert_eq!(validate(&route, &[]), Err(DeclarationError::ModelId(0)));

        route.models = vec![DeclaredModel { id: "acme flash".into(), ..Default::default() }];
        assert_eq!(
            validate(&route, &[]),
            Err(DeclarationError::ModelId(0)),
            "an id with a space passes the form but the runtime refuses it"
        );

        route.models = vec![DeclaredModel { id: "m".repeat(129), ..Default::default() }];
        assert_eq!(
            validate(&route, &[]),
            Err(DeclarationError::ModelId(0)),
            "an id past the runtime's 128-byte limit is refused at declaration"
        );

        route.models = vec![
            DeclaredModel { id: "dup".into(), ..Default::default() },
            DeclaredModel { id: "dup".into(), ..Default::default() },
        ];
        assert_eq!(validate(&route, &[]), Err(DeclarationError::DuplicateModel(1)));

        route.models = vec![DeclaredModel { id: "zero".into(), max_tokens: Some(0), ..Default::default() }];
        assert_eq!(validate(&route, &[]), Err(DeclarationError::Capacity(0, "the output limit")));

        let mut route = declared_route();
        route.api_key_env = Some("llm-pi-ai/acme".into());
        assert!(matches!(validate(&route, &[]), Err(DeclarationError::KeyRef(_))));

        route.api_key_env = None;
        route.headers = vec![("bad name".into(), "x".into())];
        assert_eq!(validate(&route, &[]), Err(DeclarationError::Header(0)));
    }

    #[test]
    fn removing_a_route_unsets_its_path_and_nothing_else() {
        assert_eq!(
            remove_route_params("acme-gateway"),
            json!({
                "ns": "llm-pi-ai",
                "ops": [{ "op": "unset", "path": ["providers", "acme-gateway"] }],
            }),
            "a route has no enabled flag: its path existing is what turns it on"
        );
    }

    #[test]
    fn protocol_choices_come_out_of_the_namespaces_own_schema() {
        let schema = json!({
            "uid": 1,
            "refs": {
                "1": { "type": "object", "dict": { "providers": 2 } },
                "2": { "type": "dict", "inner": 3 },
                "3": { "type": "object", "dict": { "api": 4 } },
                "4": { "type": "union", "list": [
                    { "type": "const", "value": "openai-completions" },
                    { "type": "const", "value": "anthropic-messages" }
                ] }
            }
        });
        let settings = json!({ "namespaces": [{ "ns": "llm-pi-ai", "schema": schema, "value": {} }] });
        assert_eq!(protocol_choices(&settings), vec!["openai-completions", "anthropic-messages"]);
        let fallback = json!({ "namespaces": [{ "ns": "llm-pi-ai", "schema": {}, "value": {} }] });
        assert_eq!(protocol_choices(&fallback), protocols().to_vec());
        assert_eq!(protocol_choices(&json!({})), protocols().to_vec());
    }

    #[test]
    fn capacities_round_trip_through_the_spellings_the_form_accepts() {
        assert_eq!(parse_capacity(""), Ok(None), "a blank field inherits");
        assert_eq!(parse_capacity(" 64000 "), Ok(Some(64_000)));
        assert_eq!(parse_capacity("256K"), Ok(Some(256_000)));
        assert_eq!(parse_capacity("1M"), Ok(Some(1_000_000)));
        assert_eq!(parse_capacity("1.5M"), Ok(Some(1_500_000)));
        assert_eq!(parse_capacity("0"), Err(()));
        assert_eq!(parse_capacity("-5"), Err(()));
        assert_eq!(parse_capacity("lots"), Err(()));
        assert_eq!(parse_capacity("12X"), Err(()));
    }

    #[test]
    fn routes_and_revision_read_off_the_settings_answer() {
        let settings = json!({ "namespaces": [
            { "ns": "llm-pi-ai", "revision": 4, "value": { "providers": {
                "acme-gateway": { "api": "openai-completions" },
                "opencode-go": {},
            } } },
        ] });
        let mut declared = routes_in_ns(&settings, "llm-pi-ai");
        declared.sort();
        assert_eq!(declared, vec!["acme-gateway", "opencode-go"]);
        assert_eq!(namespace_revision(&settings, "llm-pi-ai"), Some(4));
        assert_eq!(namespace_revision(&settings, "missing"), None);
        assert!(routes_in_ns(&json!({}), "llm-pi-ai").is_empty());
    }

    /// A scratch catalog directory for the reasoning-level tests.
    fn scratch_catalog(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("zeron-dsh-providers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn route(id: &str, ns: &str) -> GatewayProvider {
        GatewayProvider {
            id: id.to_string(),
            display_name: id.to_string(),
            settings_ns: ns.to_string(),
            api_key_env: None,
            default_base_url: None,
            account: false,
            declared: false,
            chatgpt: false,
            oauth: false,
        }
    }

    #[tokio::test]
    async fn reasoning_levels_mirror_the_catalog_and_profile_declarations() {
        let dir = scratch_catalog("levels");
        // The installed catalog declares support per model: a map pins the
        // offered levels to their wire spellings, no map offers the base
        // levels (so `max` stays out), and `reasoning: false` offers none.
        std::fs::write(
            dir.join("acme.json"),
            json!({ "openai-responses": {
                "chat:thinker": { "id": "thinker", "type": "chat",
                    "reasoning": true,
                    "thinkingLevelMap": { "off": null, "xhigh": "xhigh", "max": "max" } },
                "chat:base": { "id": "base", "type": "chat", "reasoning": true },
                "chat:plain": { "id": "plain", "type": "chat", "reasoning": false },
                "classifier:sorter": { "id": "sorter", "type": "classifier", "reasoning": true },
            } })
            .to_string(),
        )
        .unwrap();
        let mut catalogs = std::collections::HashMap::new();
        let acme = route("acme", "llm-pi-ai");
        assert_eq!(
            route_reasoning_levels(&acme, "thinker", None, &dir, &mut catalogs).await,
            vec![ReasoningLevel::XHigh, ReasoningLevel::Max]
        );
        assert_eq!(
            route_reasoning_levels(&acme, "base", None, &dir, &mut catalogs).await,
            vec![
                ReasoningLevel::Minimal,
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High
            ]
        );
        assert!(
            route_reasoning_levels(&acme, "plain", None, &dir, &mut catalogs)
                .await
                .is_empty()
        );
        assert!(
            route_reasoning_levels(&acme, "sorter", None, &dir, &mut catalogs)
                .await
                .is_empty(),
            "a classifier catalog entry never reaches a chat picker"
        );
        // A stored modelOverrides declaration decides the set over the
        // catalog; `false` takes the control away entirely.
        let settings = json!({ "namespaces": [{ "ns": "llm-pi-ai", "value": { "providers": { "acme": {
            "modelOverrides": {
                "thinker": { "reasoningEfforts": { "off": null, "high": "high" } },
                "base": { "reasoningEfforts": false },
            }
        } } } }] });
        assert_eq!(
            route_reasoning_levels(&acme, "thinker", Some(&settings), &dir, &mut catalogs).await,
            vec![ReasoningLevel::High]
        );
        assert!(
            route_reasoning_levels(&acme, "base", Some(&settings), &dir, &mut catalogs)
                .await
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reasoning_levels_declared_route_and_deepseek_switch() {
        let dir = scratch_catalog("declared");
        let mut catalogs = std::collections::HashMap::new();
        // A hand-declared route carries no installed catalog; its models[]
        // entries' reasoningEfforts are the whole declaration, with only the
        // levels the harness can run offered.
        let declared = route("acme-gateway", "llm-pi-ai");
        let settings = json!({ "namespaces": [{ "ns": "llm-pi-ai", "value": { "providers": { "acme-gateway": {
            "models": [
                { "id": "thinking", "reasoningEfforts": { "off": null, "minimal": "min", "high": "high" } },
                { "id": "silent", "reasoningEfforts": false },
                { "id": "inherit" },
            ]
        } } } }] });
        assert_eq!(
            route_reasoning_levels(&declared, "thinking", Some(&settings), &dir, &mut catalogs)
                .await,
            vec![ReasoningLevel::Minimal, ReasoningLevel::High]
        );
        assert!(
            route_reasoning_levels(&declared, "silent", Some(&settings), &dir, &mut catalogs)
                .await
                .is_empty()
        );
        assert!(
            route_reasoning_levels(&declared, "inherit", Some(&settings), &dir, &mut catalogs)
                .await
                .is_empty(),
            "no catalog exists to inherit from; nothing is invented"
        );
        // A non-pi-ai, non-deepseek route offers nothing.
        assert!(
            route_reasoning_levels(&route("x", "other-ns"), "m", None, &dir, &mut catalogs)
                .await
                .is_empty()
        );
        // llm-deepseek routes offer the adapter's efforts while thinking is
        // enabled, and none once the switch is disabled.
        let deepseek = route("deepseek-official", "llm-deepseek-api-key");
        assert_eq!(
            route_reasoning_levels(&deepseek, "deepseek-v4-pro", None, &dir, &mut catalogs).await,
            vec![ReasoningLevel::Low, ReasoningLevel::High, ReasoningLevel::Max]
        );
        let disabled = json!({ "namespaces": [{ "ns": "llm-deepseek-api-key", "value": { "thinking": "disabled" } }] });
        assert!(
            route_reasoning_levels(&deepseek, "deepseek-v4-pro", Some(&disabled), &dir, &mut catalogs)
                .await
                .is_empty()
        );
    }

    /// The SDK discovery answer the account's list is measured against: the
    /// installed catalog's 34 entries.
    fn catalog_34() -> Vec<ModelChoice> {
        (1..=34)
            .map(|index| ModelChoice {
                id: format!("catalog-model-{index}"),
                label: format!("Catalog Model {index}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn a_configured_route_lists_the_accounts_models_not_the_catalog() {
        let account = vec![
            ModelChoice { id: "mistral-medium-3".into(), label: "Mistral Medium 3".into() },
            ModelChoice { id: "zai-glm-5".into(), label: "ZAI GLM-5".into() },
        ];
        let (models, source) = account_or_catalog(Ok(account), catalog_34());
        assert_eq!(
            models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(),
            vec!["mistral-medium-3", "zai-glm-5"],
            "the route's saved key serves the account's own models — this account's API \
             answers 62, including these — not the installed catalog's 34"
        );
        assert_eq!(
            source,
            ModelSource::Account,
            "an answered account read is the picker's list, not a fallback"
        );
    }

    #[tokio::test]
    async fn a_refused_account_read_falls_back_to_the_catalog_with_its_reason() {
        let catalog = catalog_34();
        let (models, source) = account_or_catalog(Err("401".to_string()), catalog.clone());
        assert_eq!(
            models, catalog,
            "the account's refusal leaves the SDK's discovered catalog in place"
        );
        assert_eq!(
            source,
            ModelSource::CatalogFallback("401".to_string()),
            "the refusal is carried, not dropped: the picker can say why this is the catalog"
        );
    }

    #[test]
    fn a_route_that_can_oauth_still_reads_its_saved_key() {
        // Mistral's catalog entry parses to a route carrying both a browser
        // sign-in and a stored apiKeyEnv; the ChatGPT subscription's carries
        // only the oauth shape — no env ref to read an account with.
        let routes = parse_provider_catalog(&json!([
            { "provider": "mistral", "settingsNs": "llm-pi-ai",
              "nativeAuth": { "env": ["MISTRAL_API_KEY"], "oauth": true } },
            { "provider": "openai", "settingsNs": "llm-pi-ai",
              "nativeAuth": { "oauth": true } },
        ]));
        let mistral = &routes[0];
        assert_eq!(mistral.credential_ref(), Some("MISTRAL_API_KEY"));
        assert!(mistral.oauth && !mistral.chatgpt);
        // The picker's own decision — the code path route_models takes, not
        // a restated condition — reads the account for a route whose key is
        // stored, browser sign-in or none.
        let (models, source) = route_choices(
            mistral,
            true,
            Ok(vec![ModelChoice { id: "mistral-medium-3".into(), label: "Mistral Medium 3".into() }]),
            catalog_34(),
        );
        assert_eq!(
            models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(),
            vec!["mistral-medium-3"],
            "a browser sign-in layered on a saved key must not hide the account's models"
        );
        assert_eq!(source, ModelSource::Account);
        // The ChatGPT subscription carries no ref and keeps discovery,
        // whatever its credential flag reads.
        let chatgpt = &routes[1];
        assert!(chatgpt.chatgpt && chatgpt.credential_ref().is_none());
        for credential_ready in [false, true] {
            let (models, source) =
                route_choices(chatgpt, credential_ready, Err("no key".to_string()), catalog_34());
            assert_eq!(models, catalog_34());
            assert_eq!(
                source,
                ModelSource::Discovery,
                "a route with no saved ref keeps discovery — never the account, and no \
                 fallback label"
            );
        }
    }

    /// DeepSeek's own route, the ChatGPT subscription and an oauth route
    /// with no stored ref never read an account — discovery is their list
    /// outright, nothing was attempted and nothing failed — so none of
    /// their rows carry a hint, whatever `account` holds for the read that
    /// never happened.
    #[test]
    fn a_route_that_never_reads_an_account_carries_no_label() {
        let routes = parse_provider_catalog(&json!([
            { "provider": "deepseek-official", "displayName": "DeepSeek", "settingsNs": "llm-deepseek" },
            { "provider": "openai", "settingsNs": "llm-pi-ai",
              "nativeAuth": { "oauth": true } },
            { "provider": "mistral", "settingsNs": "llm-pi-ai",
              "nativeAuth": { "env": ["MISTRAL_API_KEY"], "oauth": true } },
        ]));
        let deepseek = &routes[0];
        assert!(!deepseek.chatgpt && deepseek.credential_ref().is_none());
        let chatgpt = &routes[1];
        assert!(chatgpt.chatgpt && chatgpt.credential_ref().is_none());
        let mistral = &routes[2];
        assert!(mistral.oauth && mistral.credential_ref() == Some("MISTRAL_API_KEY"));
        // `credential_ready: false`: DeepSeek and ChatGPT carry no env ref
        // at all, mistral's is not stored — the read was never attempted.
        // A refusal, an empty answer, even models: whatever `account` holds
        // for a route that never asked, it is set aside, not labelled.
        let accounts: [Result<Vec<ModelChoice>, String>; 3] = [
            Ok(Vec::new()),
            Err("no key".to_string()),
            Ok(vec![ModelChoice { id: "a".into(), label: "A".into() }]),
        ];
        for route in [deepseek, chatgpt, mistral] {
            for account in accounts.clone() {
                let rows = route_rows(route, false, account, catalog_34());
                assert_eq!(
                    rows.iter().map(|(choice, _)| choice.clone()).collect::<Vec<_>>(),
                    catalog_34(),
                    "{}: the discovered catalog is the route's list outright: {rows:?}",
                    route.id,
                );
                assert!(
                    rows.iter().all(|(_, hint)| hint.is_none()),
                    "{}: a route that never read an account labels nothing: {rows:?}",
                    route.id,
                );
            }
        }
    }

    /// A route that reads its saved key and is refused — or is answered
    /// with nothing — shows the discovered catalog labelled with why it is
    /// not the account's list. The account's own answer labels nothing.
    #[test]
    fn a_refused_account_read_labels_the_catalog() {
        let routes = parse_provider_catalog(&json!([
            { "provider": "mistral", "settingsNs": "llm-pi-ai",
              "nativeAuth": { "env": ["MISTRAL_API_KEY"], "oauth": true } },
        ]));
        let mistral = &routes[0];
        let cases: [(Result<Vec<ModelChoice>, String>, &str); 2] = [
            (Err("401".to_string()), "Shipped catalog — 401"),
            (Ok(Vec::new()), "Shipped catalog — empty"),
        ];
        for (account, hint) in cases {
            let rows = route_rows(mistral, true, account, catalog_34());
            assert_eq!(rows.len(), 34);
            assert!(
                rows.iter().all(|(_, row)| row.as_deref() == Some(hint)),
                "every catalog row carries the reason it is not the account's list: {rows:?}"
            );
        }
        let account_models = vec![
            ModelChoice { id: "mistral-medium-3".into(), label: "Mistral Medium 3".into() },
            ModelChoice { id: "zai-glm-5".into(), label: "ZAI GLM-5".into() },
        ];
        let rows = route_rows(mistral, true, Ok(account_models.clone()), catalog_34());
        assert_eq!(
            rows,
            account_models
                .into_iter()
                .map(|choice| (choice, None))
                .collect::<Vec<(ModelChoice, Option<String>)>>(),
            "the account's own list carries no hint"
        );
    }

    /// A route that keeps discovery — DeepSeek's own, the ChatGPT
    /// subscription, an oauth route with no saved ref — is not a catalog
    /// fallback: its rows carry no "Shipped catalog" label.
    #[test]
    fn a_route_that_keeps_discovery_is_not_labelled_a_catalog_fallback() {
        let routes = parse_provider_catalog(&json!([
            { "provider": "deepseek", "settingsNs": "llm-deepseek" },
            { "provider": "openai", "settingsNs": "llm-pi-ai",
              "nativeAuth": { "oauth": true } },
            { "provider": "mistral", "settingsNs": "llm-pi-ai",
              "nativeAuth": { "oauth": true } },
        ]));
        let discovered = vec![
            ModelChoice { id: "catalog-a".into(), label: "Catalog A".into() },
            ModelChoice { id: "catalog-b".into(), label: "Catalog B".into() },
        ];
        for route in &routes {
            let (models, source) =
                route_choices(route, true, Err("not read".to_string()), discovered.clone());
            assert_eq!(models, discovered, "{} keeps its discovery list", route.id);
            if let ModelSource::CatalogFallback(reason) = source {
                panic!(
                    "{}: a route that keeps discovery was labelled a catalog fallback: {reason}",
                    route.id
                );
            }
        }
    }

    /// Only a catalog fallback carries a hint: the account's own list and a
    /// route that keeps discovery say nothing extra.
    #[test]
    fn only_a_catalog_fallback_carries_a_label() {
        assert_eq!(description_of(&ModelSource::Account), None);
        assert_eq!(description_of(&ModelSource::Discovery), None);
        assert_eq!(
            description_of(&ModelSource::CatalogFallback("401".to_string())),
            Some("Shipped catalog — 401".to_string())
        );
    }

    /// The picker is a chat picker: `capabilities.completion_chat` decides.
    /// Only an explicit `false` drops a row — an unflagged row, or one whose
    /// flag is not a boolean, is the account's until the payload says
    /// otherwise, whether or not the discovered catalog names its id.
    #[test]
    fn a_non_chat_account_model_is_not_offered() {
        let payload = json!({ "data": [
            { "id": "chat-model", "capabilities": { "completion_chat": true } },
            { "id": "embed-model", "capabilities": { "completion_chat": false } },
            { "id": "unflagged-known" },
            { "id": "unflagged-stray" },
            { "id": "flagged-nonbool", "capabilities": { "completion_chat": "yes" } },
        ] });
        let models = account_chat_models(&payload);
        assert_eq!(
            models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(),
            vec!["chat-model", "unflagged-known", "unflagged-stray", "flagged-nonbool"],
            "an unflagged row is the account's until the payload says otherwise",
        );
    }

    #[tokio::test]
    async fn a_saved_credential_is_read_back_on_a_cold_start() {
        let home = tempfile::tempdir().expect("a temp home");
        let marker = "cold-start-marker-9f4b2e";
        std::fs::write(
            home.path().join(".credentials.yaml"),
            format!(
                "version: 1\n\
                 records:\n  client-connection/browser-session:\n    kind: mention\n    payload:\n      version: 1\n      secret: not-{marker}\n\
                 refs:\n  MARKER_ACCOUNT_API_KEY: {marker}\n  QUOTED_KEY: \"quoted-{marker}\"\n  EMPTY_KEY:\n"
            ),
        )
        .expect("the credential store the gateway leaves behind");
        // The reader is the cold-start path: no process holds the key, the
        // file is all there is.
        assert_eq!(
            stored_key(home.path(), "MARKER_ACCOUNT_API_KEY").as_deref(),
            Some(marker),
            "a ref saved under refs: reads back verbatim"
        );
        assert_eq!(
            stored_key(home.path(), "QUOTED_KEY").as_deref(),
            Some(format!("quoted-{marker}").as_str()),
            "a quoted scalar reads back unquoted"
        );
        assert_eq!(stored_key(home.path(), "EMPTY_KEY"), None, "an empty value is no key");
        // Nothing outside refs: is a ref, whatever it is called.
        assert_eq!(stored_key(home.path(), "secret"), None);
        assert_eq!(stored_key(home.path(), "client-connection/browser-session"), None);
        // A miss and a missing file are both a plain None, so no error
        // string exists for the marker to ride out in.
        let miss = stored_key(home.path(), "ABSENT_API_KEY");
        assert_eq!(miss, None);
        assert!(!format!("{miss:?}").contains(marker));
        assert_eq!(
            stored_key(&home.path().join("never-written"), "MARKER_ACCOUNT_API_KEY"),
            None,
            "a cold start with no store file is no key, not an error"
        );
    }

}
