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
    /// Whether the route is one someone declared by hand rather than one pi-ai
    /// ships (`declared` in the catalog answer), which is the only kind Zeron
    /// may remove: a catalog route's path always exists.
    pub declared: bool,
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
                declared: entry["declared"].as_bool().unwrap_or(false),
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
/// with a letter. DSH's own custom-provider card enforces the same grammar
/// before it writes, and the credential seam reinforces it: the derived ref is
/// upper-cased, so a digit-leading id would name a ref the store refuses.
pub fn route_id_valid(id: &str) -> bool {
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

/// Whether `value` is an absolute http(s) endpoint prefix.
pub fn http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
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
                "{id} is not a route id: lower-case segments joined by dashes, starting with a letter (acme-gateway)."
            ),
            Self::RouteTaken(id) => format!("{id} is already a route on this device."),
            Self::BaseUrl(url) => format!("{url} is not an http(s) endpoint."),
            Self::Api(api) => format!("{api} is not a protocol this adapter serves."),
            Self::NoModels => "a declared route needs at least one model: it has no catalog to inherit.".into(),
            Self::ModelId(index) => format!("model {} needs an id.", index + 1),
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
        let id = model.id.trim();
        if id.is_empty() || id != model.id {
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
        for invalid in ["", "Acme", "acme_gateway", "-acme", "acme-", "acme--gateway", "9acme", "acme gateway"] {
            assert!(!route_id_valid(invalid), "{invalid} is not route-shaped");
        }
        assert_eq!(key_ref_for("acme-gateway"), "ACME_GATEWAY_API_KEY");
        assert_eq!(key_ref_for("a1"), "A1_API_KEY");
        assert!(key_ref_valid("ACME_GATEWAY_API_KEY"));
        assert!(!key_ref_valid("llm-pi-ai/opencode-go"), "a record key is not an env ref");
        assert!(!key_ref_valid("9ACME"));
        assert!(http_url("http://127.0.0.1:41999/v1") && http_url("https://x.test"));
        assert!(!http_url("ftp://x.test") && !http_url("gateway.test/v1"));
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
}
