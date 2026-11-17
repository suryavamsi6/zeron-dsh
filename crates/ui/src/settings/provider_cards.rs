//! Provider route cards for the routes DSH's gateway configures.
//!
//! The SDK's JSON-RPC profile only runs sessions; provider, credential,
//! account and model settings live on DSH's app gateway. These cards are the
//! app's read/write over that surface: `DshProviders` for the catalog, then
//! per route `DshCredentials`/`DshSetCredential`/`DshUnsetCredential` for the
//! key, `DshAccount`/`DshSignIn`/`DshSignOut` for the DeepSeek account,
//! `DshChatgpt*` for Zeron's own ChatGPT plugin, and `DshSettings`/
//! `DshMutateSettings`/`DshDiscoverModels` for models and the default model.
//! Argument and answer shapes are parsed by `zeron_engine::dsh_providers`.

use std::collections::HashMap;

use gpui::{AnyElement, Context, Entity, Render, Task, Window, div, prelude::*, px};
use serde_json::Value;
use zeron_engine::dsh_providers::{self, GatewayProvider, ModelChoice, TestMode};
use zeron_proto::HarnessId;
use zeron_rpc::methods;

use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    popover::{self, Loadable},
    settings::{
        accounts,
        composer::ComposerDefaults,
        widgets::{self, ActionTone},
    },
    state::AppState,
    theme::Theme,
};

/// The env-shaped credential ref of the official DeepSeek route.
pub const DEEPSEEK_API_KEY_REF: &str = "DEEPSEEK_API_KEY";

/// One route's live state on its card.
struct CardState {
    credentials: Loadable<Value>,
    account: Loadable<Value>,
    chatgpt: Loadable<Value>,
    models: Loadable<Vec<ModelChoice>>,
    input: Entity<ComposerInput>,
    editing_key: bool,
    busy: Option<&'static str>,
    test_result: Option<String>,
    sign_in: Option<(Option<String>, Option<String>)>,
    task: Option<Task<()>>,
    _events: gpui::Subscription,
}

/// The provider cards section on the Providers settings page.
pub struct ProviderCards {
    state: Entity<AppState>,
    target_device: Option<String>,
    catalog: Loadable<Vec<GatewayProvider>>,
    settings: Loadable<Value>,
    cards: HashMap<String, CardState>,
    error: Option<String>,
    load_task: Option<Task<()>>,
}

impl ProviderCards {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            state,
            target_device: None,
            catalog: Loadable::Idle,
            settings: Loadable::Idle,
            cards: HashMap::new(),
            error: None,
            load_task: None,
        };
        page.load(cx);
        page
    }

    pub(crate) fn set_target_device(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if self.target_device == target {
            cx.notify();
            return;
        }
        self.target_device = target;
        self.load(cx);
        cx.notify();
    }

    fn with_target(&self, mut value: Value) -> Value {
        if let (Some(target), Some(object)) = (&self.target_device, value.as_object_mut()) {
            object.insert("targetDeviceId".into(), serde_json::json!(target));
        }
        value
    }

    fn engine(&self, cx: &Context<Self>) -> Option<crate::state::EngineHandle> {
        self.state.read(cx).engine().cloned()
    }

    fn card(&mut self, id: &str, cx: &mut Context<Self>) -> &mut CardState {
        if !self.cards.contains_key(id) {
            let input = cx.new(|cx| {
                ComposerInput::new(format!("{id} API key"), cx)
                    .with_single_line()
                    .with_masked()
            });
            let submit_id = id.to_string();
            let events = cx.subscribe(&input, move |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.save_key(&submit_id, cx);
                }
            });
            self.cards.insert(
                id.to_string(),
                CardState {
                    credentials: Loadable::Idle,
                    account: Loadable::Idle,
                    chatgpt: Loadable::Idle,
                    models: Loadable::Idle,
                    input,
                    editing_key: false,
                    busy: None,
                    test_result: None,
                    sign_in: None,
                    task: None,
                    _events: events,
                },
            );
        }
        self.cards.get_mut(id).expect("just inserted")
    }

    /// Load the catalog and the settings store, then each route's state.
    pub(crate) fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            self.error = Some("Connect to the run device to see its providers.".into());
            cx.notify();
            return;
        };
        self.catalog = Loadable::Loading;
        self.settings = Loadable::Loading;
        let target = self.target_device.clone();
        let providers_params = self.with_target(serde_json::json!({}));
        let settings_params = providers_params.clone();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let client = engine.client();
            let (providers, settings) = futures::future::join(
                client.call(methods::DSH_PROVIDERS, providers_params),
                client.call(methods::DSH_SETTINGS, settings_params),
            )
            .await;
            let catalog = providers
                .map(|value| dsh_providers::parse_provider_catalog(&value))
                .map_err(|error| error.to_string());
            let settings = settings.map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                match (&catalog, &settings) {
                    (Ok(catalog), Ok(settings)) => {
                        page.catalog = Loadable::Ready(catalog.clone());
                        page.settings = Loadable::Ready(settings.clone());
                        let ids: Vec<String> = catalog.iter().map(|p| p.id.clone()).collect();
                        for id in ids {
                            page.load_card(&id, settings, cx);
                        }
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        page.error = Some(format!("The provider service didn't answer: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// One route's credential, account and ChatGPT state, plus its models
    /// read offline from the settings store.
    fn load_card(&mut self, id: &str, settings: &Value, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let Some(provider) = self
            .catalog
            .ready()
            .and_then(|catalog| catalog.iter().find(|p| p.id == id).cloned())
        else {
            return;
        };
        {
            let card = self.card(id, cx);
            card.credentials = Loadable::Loading;
            card.account = Loadable::Loading;
            card.chatgpt = Loadable::Loading;
            card.models =
                Loadable::Ready(dsh_providers::models_in_ns(settings, &provider.settings_ns));
        }
        let target = self.target_device.clone();
        let credential_ref = provider.credential_ref().map(str::to_string);
        let want_account = provider.account;
        let want_chatgpt = provider.chatgpt;
        let creds_params = self.with_target(dsh_providers::describe_credentials_params(
            &[credential_ref.as_deref().unwrap_or("")],
        ));
        let account_params = self.with_target(serde_json::json!({}));
        let chatgpt_params = self.with_target(serde_json::json!({}));
        let pid = id.to_string();
        cx.spawn(async move |this, cx| {
            let client = engine.client();
            let creds = if credential_ref.is_some() {
                client
                    .call(methods::DSH_CREDENTIALS, creds_params)
                    .await
                    .map_err(|error| error.to_string())
            } else {
                Err("no credential ref".into())
            };
            let account = if want_account {
                client
                    .call(methods::DSH_ACCOUNT, account_params)
                    .await
                    .map_err(|error| error.to_string())
            } else {
                Err("not an account route".into())
            };
            let chatgpt = if want_chatgpt {
                client
                    .call(methods::DSH_CHATGPT_STATUS, chatgpt_params)
                    .await
                    .map_err(|error| error.to_string())
            } else {
                Err("not the chatgpt route".into())
            };
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                if credential_ref.is_some() {
                    card.credentials = match creds {
                        Ok(value) => Loadable::Ready(value),
                        Err(error) => Loadable::Error(error),
                    };
                }
                if want_account {
                    card.account = match account {
                        Ok(value) => Loadable::Ready(value),
                        Err(error) => Loadable::Error(error),
                    };
                }
                if want_chatgpt {
                    card.chatgpt = match chatgpt {
                        Ok(value) => Loadable::Ready(value),
                        Err(error) => Loadable::Error(error),
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reload_card_state(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(settings) = self.settings.ready().cloned() {
            self.load_card(id, &settings, cx);
        }
    }

    fn save_key(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let Some(provider) = self
            .catalog
            .ready()
            .and_then(|catalog| catalog.iter().find(|p| p.id == id).cloned())
        else {
            return;
        };
        let Some(key_ref) = provider.credential_ref().map(str::to_string) else { return };
        let key = self.card(id, cx).input.read(cx).text().trim().to_owned();
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            self.card(id, cx).test_result = Some("Enter an API key without whitespace.".into());
            cx.notify();
            return;
        }
        let params = self.with_target(dsh_providers::set_credential_params(&key_ref, &key));
        let target = self.target_device.clone();
        let pid = id.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Saving…");
        card.editing_key = false;
        card.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_SET_CREDENTIAL, params)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                match result {
                    Ok(_) => {
                        card.input.update(cx, |input, cx| input.set_text("", cx));
                        card.test_result =
                            Some("Key saved to Zeron's shared store; the next run reads it.".into());
                        page.reload_card_state(&pid, cx);
                    }
                    Err(error) => {
                        card.test_result = Some(format!("Couldn't save the key: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn remove_key(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let Some(key_ref) = self
            .catalog
            .ready()
            .and_then(|catalog| catalog.iter().find(|p| p.id == id))
            .and_then(|p| p.credential_ref().map(str::to_string))
        else {
            return;
        };
        let params = self.with_target(dsh_providers::unset_credential_params(&key_ref));
        let target = self.target_device.clone();
        let pid = id.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Removing…");
        card.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_UNSET_CREDENTIAL, params)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                card.test_result = Some(match result {
                    Ok(_) => "Key removed from the store.".into(),
                    Err(error) => format!("Couldn't remove the key: {error}"),
                });
                page.reload_card_state(&pid, cx);
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl ProviderCards {
    /// Test connection: offline reads the store only; live makes the one
    /// provider call (`DshDiscoverModels`) and is labeled billable.
    fn run_test(&mut self, id: &str, mode: TestMode, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let Some(provider) = self
            .catalog
            .ready()
            .and_then(|catalog| catalog.iter().find(|p| p.id == id).cloned())
        else {
            return;
        };
        let calls = mode.calls(provider.credential_ref(), &provider.settings_ns);
        let target = self.target_device.clone();
        let pid = id.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Testing…");
        card.test_result = None;
        card.task = Some(cx.spawn(async move |this, cx| {
            let client = engine.client();
            let mut lines = Vec::new();
            let mut failed = None;
            for (method, params) in calls {
                let params = if let Some(target) = &target {
                    let mut params = params;
                    if let Some(object) = params.as_object_mut() {
                        object.insert("targetDeviceId".into(), serde_json::json!(target));
                    }
                    params
                } else {
                    params
                };
                match client.call(method, params).await {
                    Ok(value) => {
                        if method == methods::DSH_CREDENTIALS {
                            let key_ref = provider.credential_ref().unwrap_or_default();
                            lines.push(format!(
                                "credential {key_ref}: {}",
                                if dsh_providers::credential_configured(&value, key_ref) {
                                    "saved"
                                } else {
                                    "not saved"
                                }
                            ));
                        } else if method == methods::DSH_SETTINGS {
                            lines.push(format!(
                                "settings namespaces: {}",
                                dsh_providers::settings_namespaces(&value).len()
                            ));
                        } else {
                            lines.push(format!(
                                "provider answered with {} models",
                                dsh_providers::parse_models(&value).len()
                            ));
                        }
                    }
                    Err(error) => {
                        failed = Some(error.to_string());
                        break;
                    }
                }
            }
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                card.test_result = Some(match failed {
                    Some(error) => format!("{}: failed — {error}", mode.label()),
                    None => format!("{}: {}", mode.label(), lines.join("; ")),
                });
                if mode == TestMode::Live {
                    page.reload_card_state(&pid, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn start_chatgpt_sign_in(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let params = self.with_target(serde_json::json!({}));
        let target = self.target_device.clone();
        let pid = id.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Starting sign-in…");
        card.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_CHATGPT_START, params)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                match result {
                    Ok(value) => {
                        let url = dsh_providers::parse_chatgpt_start(&value);
                        card.sign_in = Some((url.clone(), None));
                        if let Some(url) = &url {
                            accounts::open_login_url(url, cx);
                        }
                        card.test_result = Some(
                            "Sign-in started — finish it in the browser tab that opened.".into(),
                        );
                        page.poll_chatgpt(&pid, cx);
                    }
                    Err(error) => {
                        card.test_result = Some(format!("Couldn't start the sign-in: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn poll_chatgpt(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let params = self.with_target(serde_json::json!({}));
        let target = self.target_device.clone();
        let pid = id.to_string();
        cx.spawn(async move |this, cx| {
            let client = engine.client();
            for _ in 0..150 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let status = client
                    .call(methods::DSH_CHATGPT_STATUS, params.clone())
                    .await;
                let done = this
                    .update(cx, |page, cx| {
                        if page.target_device != target {
                            return true;
                        }
                        let Some(card) = page.cards.get_mut(&pid) else {
                            return true;
                        };
                        match status {
                            Ok(value) => {
                                let status = dsh_providers::parse_chatgpt_status(&value);
                                card.chatgpt = Loadable::Ready(value);
                                if status.authorized || !status.running {
                                    card.sign_in = None;
                                    card.test_result = Some(if status.authorized {
                                        "ChatGPT signed in. The grant lives in Zeron's shared store."
                                            .into()
                                    } else {
                                        "Sign-in not completed.".into()
                                    });
                                    cx.notify();
                                    return true;
                                }
                            }
                            Err(_) => return true,
                        }
                        false
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    fn cancel_chatgpt_sign_in(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let params = self.with_target(serde_json::json!({}));
        let target = self.target_device.clone();
        let pid = id.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Cancelling…");
        card.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_CHATGPT_CANCEL, params)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                card.sign_in = None;
                card.test_result = Some(match result {
                    Ok(_) => "Sign-in cancelled; the callback port is released.".into(),
                    Err(error) => format!("Couldn't cancel: {error}"),
                });
                page.reload_card_state(&pid, cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn start_account_sign_in(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let params = self.with_target(serde_json::json!({}));
        let target = self.target_device.clone();
        let pid = id.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Starting sign-in…");
        card.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_SIGN_IN, params)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                match result {
                    Ok(value) => {
                        let start = dsh_providers::parse_sign_in_start(&value);
                        card.sign_in = Some((start.url.clone(), start.attempt_id.clone()));
                        if let Some(url) = &start.url {
                            accounts::open_login_url(url, cx);
                        }
                        card.test_result =
                            Some("Sign-in started — the callback returns to this app.".into());
                        page.poll_account(&pid, cx);
                    }
                    Err(error) => {
                        card.test_result = Some(format!("Couldn't start the sign-in: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn poll_account(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let params = self.with_target(serde_json::json!({}));
        let target = self.target_device.clone();
        let pid = id.to_string();
        cx.spawn(async move |this, cx| {
            let client = engine.client();
            for _ in 0..150 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let state = client.call(methods::DSH_ACCOUNT, params.clone()).await;
                let done = this
                    .update(cx, |page, cx| {
                        if page.target_device != target {
                            return true;
                        }
                        let Some(card) = page.cards.get_mut(&pid) else {
                            return true;
                        };
                        match state {
                            Ok(value) => {
                                let account = dsh_providers::parse_account_state(&value);
                                card.account = Loadable::Ready(value);
                                if account.signed_in {
                                    card.sign_in = None;
                                    card.test_result =
                                        Some("Signed in to the DeepSeek account.".into());
                                    cx.notify();
                                    return true;
                                }
                            }
                            Err(_) => return true,
                        }
                        false
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    fn cancel_account_sign_in(&mut self, id: &str, cx: &mut Context<Self>) {
        let attempt_id = self
            .cards
            .get(id)
            .and_then(|card| card.sign_in.as_ref())
            .and_then(|(_, attempt)| attempt.clone());
        let Some(attempt_id) = attempt_id else { return };
        let Some(engine) = self.engine(cx) else { return };
        let params = self.with_target(serde_json::json!({ "attemptId": attempt_id }));
        let target = self.target_device.clone();
        let pid = id.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Cancelling…");
        card.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_CANCEL_SIGN_IN, params)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                card.sign_in = None;
                card.test_result = Some(match result {
                    Ok(_) => "Sign-in attempt cancelled.".into(),
                    Err(error) => format!("Couldn't cancel: {error}"),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn sign_out(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let params = self.with_target(serde_json::json!({}));
        let target = self.target_device.clone();
        let pid = id.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Signing out…");
        card.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_SIGN_OUT, params)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                card.test_result = Some(match result {
                    Ok(_) => "Signed out.".into(),
                    Err(error) => format!("Couldn't sign out: {error}"),
                });
                page.reload_card_state(&pid, cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Set the default model in DSH's `agent-default-model` namespace; runs
    /// with no model of their own pick it up from the shared store.
    fn set_default_model(&mut self, id: &str, model: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let params = self.with_target(dsh_providers::set_default_model_params(id, model));
        let target = self.target_device.clone();
        let pid = id.to_string();
        let model = model.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Saving default…");
        card.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_MUTATE_SETTINGS, params)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                card.test_result = Some(match result {
                    Ok(_) => format!("Default model set to {model}."),
                    Err(error) => format!("Couldn't set the default model: {error}"),
                });
                page.reload_settings(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn reload_settings(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let params = self.with_target(serde_json::json!({}));
        let target = self.target_device.clone();
        self.settings = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_SETTINGS, params)
                .await
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                match result {
                    Ok(value) => {
                        page.settings = Loadable::Ready(value.clone());
                        let catalog = page.catalog.ready().cloned().unwrap_or_default();
                        for provider in &catalog {
                            if let Some(card) = page.cards.get_mut(&provider.id) {
                                card.models = Loadable::Ready(dsh_providers::models_in_ns(
                                    &value,
                                    &provider.settings_ns,
                                ));
                            }
                        }
                    }
                    Err(error) => page.error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Live model refresh: the one provider-contacting call, labeled as such.
    fn refresh_models(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else { return };
        let Some(provider) = self
            .catalog
            .ready()
            .and_then(|catalog| catalog.iter().find(|p| p.id == id).cloned())
        else {
            return;
        };
        let params =
            self.with_target(dsh_providers::discover_models_params(&provider.settings_ns));
        let target = self.target_device.clone();
        let pid = id.to_string();
        let card = self.card(id, cx);
        card.busy = Some("Contacting provider…");
        card.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DSH_DISCOVER_MODELS, params)
                .await
                .map(|value| dsh_providers::parse_models(&value))
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let Some(card) = page.cards.get_mut(&pid) else { return };
                card.busy = None;
                card.models = match result {
                    Ok(models) => Loadable::Ready(models),
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Star/unstar a model in the composer defaults (the picker's favorites
    /// rail reads the same file), then bump the pickers' catalog.
    fn toggle_favorite(&mut self, model: &str, cx: &mut Context<Self>) {
        let Some(dir) = crate::settings::data_dir(cx) else { return };
        let mut defaults = ComposerDefaults::load(&dir);
        defaults.toggle_favorite(HarnessId::DeepSeek, model);
        if let Err(error) = defaults.save(&dir) {
            self.error = Some(format!("Couldn't save favorites: {error}"));
        }
        crate::pickers::bump_harness_catalog(cx);
        cx.notify();
    }

    fn is_favorite(&self, cx: &Context<Self>, model: &str) -> bool {
        crate::settings::data_dir(cx)
            .map(|dir| ComposerDefaults::load(&dir).is_favorite(HarnessId::DeepSeek, model))
            .unwrap_or(false)
    }
}

impl ProviderCards {
    fn render_card(
        &mut self,
        provider: &GatewayProvider,
        ix: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = provider.id.clone();
        let key_ref = provider.credential_ref().map(str::to_string);
        let (configured, account_state, chatgpt, models, busy, test_result, editing_key, input, sign_in) = {
            let card = self.card(&id, cx);
            (
                match (&key_ref, card.credentials.ready()) {
                    (Some(r), Some(value)) => dsh_providers::credential_configured(value, r),
                    _ => false,
                },
                card.account.ready().map(dsh_providers::parse_account_state),
                card.chatgpt.ready().map(dsh_providers::parse_chatgpt_status),
                card.models.ready().cloned().unwrap_or_default(),
                card.busy,
                card.test_result.clone(),
                card.editing_key,
                card.input.clone(),
                card.sign_in.clone(),
            )
        };
        let default = self
            .settings
            .ready()
            .and_then(dsh_providers::default_model_of);

        let mut meta: Vec<AnyElement> = vec![
            div().child(format!("route {}", provider.id)).into_any_element(),
            div()
                .child(format!("settings {}", provider.settings_ns))
                .into_any_element(),
        ];
        if let Some(key_ref) = &key_ref {
            meta.push(
                div()
                    .child(if configured {
                        format!("key saved ({key_ref})")
                    } else {
                        format!("no key saved ({key_ref})")
                    })
                    .into_any_element(),
            );
        }
        if let Some(account) = &account_state {
            meta.push(
                div()
                    .child(format!(
                        "account {} ({})",
                        if account.signed_in { "signed in" } else { "signed out" },
                        account.status
                    ))
                    .into_any_element(),
            );
        }
        if let Some(chatgpt) = &chatgpt {
            meta.push(
                div()
                    .child(format!(
                        "ChatGPT {}",
                        if chatgpt.authorized {
                            "signed in"
                        } else if chatgpt.running {
                            "sign-in running"
                        } else {
                            "signed out"
                        }
                    ))
                    .into_any_element(),
            );
        }

        let mut actions = div().flex().flex_row().flex_wrap().gap(px(8.0));
        if key_ref.is_some() && !editing_key {
            let label = if configured { "Change key" } else { "Add API key" };
            let pid = id.clone();
            actions = actions.child(
                widgets::action_button(theme, ActionTone::Quiet)
                    .id(format!("provider-key-{}", pid.as_str()))
                    .on_click(cx.listener(move |page, _, _, cx| {
                        page.card(&pid, cx).editing_key = true;
                        cx.notify();
                    }))
                    .child(label),
            );
            if configured {
                let pid = id.clone();
                actions = actions.child(
                    widgets::ghost_action(theme)
                        .id(format!("provider-key-remove-{}", pid.as_str()))
                        .on_click(cx.listener(move |page, _, _, cx| {
                            page.remove_key(&pid, cx);
                        }))
                        .child("Remove"),
                );
            }
        }
        if provider.chatgpt {
            let signed_in = chatgpt.as_ref().is_some_and(|s| s.authorized);
            let running = chatgpt.as_ref().is_some_and(|s| s.running);
            let pid = id.clone();
            if running || sign_in.is_some() {
                actions = actions.child(
                    widgets::ghost_action(theme)
                        .id(format!("provider-chatgpt-cancel-{}", pid.as_str()))
                        .on_click(cx.listener(move |page, _, _, cx| {
                            page.cancel_chatgpt_sign_in(&pid, cx);
                        }))
                        .child("Cancel sign-in"),
                );
            } else {
                actions = actions.child(
                    widgets::action_button(theme, ActionTone::Quiet)
                        .id(format!("provider-chatgpt-start-{}", pid.as_str()))
                        .when(!signed_in, |el| {
                            el.on_click(cx.listener(move |page, _, _, cx| {
                                page.start_chatgpt_sign_in(&pid, cx);
                            }))
                        })
                        .child(if signed_in { "Signed in with ChatGPT" } else { "Sign in with ChatGPT" }),
                );
            }
        }
        if provider.account {
            let signed_in = account_state.as_ref().is_some_and(|a| a.signed_in);
            let pid = id.clone();
            if sign_in.is_some() && !signed_in {
                actions = actions.child(
                    widgets::ghost_action(theme)
                        .id(format!("provider-account-cancel-{}", pid.as_str()))
                        .on_click(cx.listener(move |page, _, _, cx| {
                            page.cancel_account_sign_in(&pid, cx);
                        }))
                        .child("Cancel sign-in"),
                );
            } else if signed_in {
                actions = actions.child(
                    widgets::ghost_action(theme)
                        .id(format!("provider-sign-out-{}", pid.as_str()))
                        .on_click(cx.listener(move |page, _, _, cx| {
                            page.sign_out(&pid, cx);
                        }))
                        .child("Sign out"),
                );
            } else {
                actions = actions.child(
                    widgets::action_button(theme, ActionTone::Quiet)
                        .id(format!("provider-account-start-{}", pid.as_str()))
                        .on_click(cx.listener(move |page, _, _, cx| {
                            page.start_account_sign_in(&pid, cx);
                        }))
                        .child("Sign in"),
                );
            }
        }
        for mode in [TestMode::Offline, TestMode::Live] {
            let pid = id.clone();
            actions = actions.child(
                widgets::ghost_action(theme)
                    .id(format!("provider-test-{}-{}", pid, mode.label()))
                    .tooltip(widgets::text_tooltip(mode.detail()))
                    .on_click(cx.listener(move |page, _, _, cx| {
                        page.run_test(&pid, mode, cx);
                    }))
                    .child(mode.label()),
            );
        }

        let mut card = widgets::card_row(theme, ix == 0)
            .id(format!("provider-card-{}", id.as_str()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(theme, provider.display_name.clone()))
                    .child(widgets::meta_line(theme, meta)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(widgets::badge(
                        theme,
                        if provider.chatgpt {
                            "ChatGPT"
                        } else if provider.account {
                            "account"
                        } else {
                            "API key"
                        },
                    ))
                    .child(if busy.is_some() {
                        crate::loaders::ring_spinner(
                            format!("provider-busy-{id}"),
                            12.0,
                            theme.text_muted,
                        )
                        .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            .child(actions);

        if editing_key && key_ref.is_some() {
            let pid = id.clone();
            let key_ref = key_ref.clone().unwrap_or_default();
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .mt(px(8.0))
                    .child(popover::dialog_field(input.into_any_element()))
                    .child(
                        widgets::action_button(theme, ActionTone::Solid)
                            .id(format!("provider-key-save-{}", pid.as_str()))
                            .on_click(cx.listener(move |page, _, _, cx| {
                                page.save_key(&pid, cx);
                            }))
                            .child("Save key"),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.text_muted)
                            .child(format!(
                                "Stored as {key_ref} in Zeron's shared credential store; the next run reads it."
                            )),
                    ),
            );
        }

        if let Some(result) = test_result {
            card = card.child(
                div()
                    .mt(px(8.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(result),
            );
        }

        if !models.is_empty() {
            let default_model = default
                .as_ref()
                .filter(|(provider_id, _)| *provider_id == id)
                .map(|(_, model)| model.clone());
            let mut rows = div().flex().flex_col().mt(px(8.0));
            for model in &models {
                let is_default = default_model.as_deref() == Some(model.id.as_str());
                let starred = self.is_favorite(cx, &model.id);
                let mid = model.id.clone();
                let pid = id.clone();
                rows = rows.child(
                    div()
                        .id(format!("provider-model-{pid}-{mid}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .py(px(4.0))
                        .child(
                            div()
                                .id(format!("provider-model-star-{pid}-{mid}"))
                                .cursor_pointer()
                                .role(gpui::Role::Button)
                                .aria_label(if starred { "Unstar model" } else { "Star model" })
                                .tab_index(0)
                                .on_click(cx.listener(move |page, _, _, cx| {
                                    page.toggle_favorite(&mid, cx);
                                }))
                                .child(
                                    crate::icons::icon(if starred {
                                        crate::icons::STAR_BOLD
                                    } else {
                                        crate::icons::STAR
                                    })
                                    .size(px(14.0))
                                    .text_color(if starred {
                                        theme.accent
                                    } else {
                                        theme.text_muted
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(format!("{} ({})", model.label, model.id)),
                        )
                        .when(is_default, |el| {
                            el.child(widgets::badge_active(theme, "default"))
                        })
                        .when(!is_default, |el| {
                            el.child(
                                widgets::ghost_action(theme)
                                    .id(format!("provider-model-default-{pid}-{mid}"))
                                    .on_click(cx.listener(move |page, _, _, cx| {
                                        page.set_default_model(&pid, &mid, cx);
                                    }))
                                    .child("Set default"),
                            )
                        }),
                );
            }
            let pid = id.clone();
            card = card
                .child(widgets::details_label(theme, "Models"))
                .child(rows)
                .child(
                    widgets::ghost_action(theme)
                        .id(format!("provider-models-refresh-{}", pid.as_str()))
                        .tooltip(widgets::text_tooltip(TestMode::Live.detail()))
                        .on_click(cx.listener(move |page, _, _, cx| {
                            page.refresh_models(&pid, cx);
                        }))
                        .child("Refresh models from provider (live — billable)"),
                );
        }

        card.into_any_element()
    }
}

impl Render for ProviderCards {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(widgets::details_label(&theme, "Providers on this device"))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(
                        "The routes DSH's gateway configures. A provider set up here is the \
                         provider the next run uses — the same shared store, no second \
                         configuration.",
                    ),
            );
        let catalog = self.catalog.clone();
        match &catalog {
            Loadable::Idle | Loadable::Loading => {
                body = body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(crate::loaders::ring_spinner(
                            "provider-cards-loading",
                            12.0,
                            theme.text_muted,
                        ))
                        .child("Loading providers…"),
                );
            }
            Loadable::Error(error) => {
                body = body.child(widgets::error_strip(&theme, error.clone()));
            }
            Loadable::Ready(catalog) => {
                if catalog.is_empty() {
                    body = body.child("No configurable providers on this device.");
                }
                let cards: Vec<AnyElement> = catalog
                    .iter()
                    .enumerate()
                    .map(|(ix, provider)| self.render_card(provider, ix, &theme, cx))
                    .collect();
                body = body
                    .child(widgets::section_card(&theme).children(cards))
                    .when_some(self.error.clone(), |el, error| {
                        el.child(widgets::error_strip(&theme, error))
                    });
            }
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_catalog_drives_what_a_card_offers() {
        let catalog = dsh_providers::parse_provider_catalog(&json!([
            { "provider": "deepseek-official", "displayName": "DeepSeek", "settingsNs": "llm-deepseek", "apiKeyEnv": "DEEPSEEK_API_KEY", "account": true },
            { "provider": "opencode-go", "settingsNs": "llm-pi-ai", "apiKeyEnv": "OPENCODE_API_KEY" },
            { "provider": "chatgpt", "settingsNs": "llm-pi-ai", "auth": "chatgpt" },
        ]));
        assert_eq!(catalog.len(), 3);
        assert_eq!(catalog[0].credential_ref(), Some(DEEPSEEK_API_KEY_REF));
        assert!(catalog[0].account);
        assert!(catalog[2].chatgpt);
    }

    #[test]
    fn test_modes_stay_labeled_with_their_cost() {
        assert_eq!(TestMode::Offline.label(), "Test connection (offline)");
        assert!(TestMode::Offline.detail().contains("cannot bill"));
        assert_eq!(TestMode::Live.label(), "Test connection (live — billable)");
        assert!(TestMode::Live.detail().contains("metered or billed"));
    }

    #[test]
    fn the_default_model_round_trips_through_settings_params() {
        let settings = json!({ "namespaces": [
            { "ns": "agent-default-model", "value": { "provider": "opencode-go", "model": "gpt-5" } },
        ] });
        assert_eq!(
            dsh_providers::default_model_of(&settings).as_ref(),
            Some(&("opencode-go".to_string(), "gpt-5".to_string()))
        );
        let params = dsh_providers::set_default_model_params("opencode-go", "gpt-5");
        assert_eq!(params["ns"], "agent-default-model");
        assert_eq!(params["ops"][0]["path"], json!(["provider"]));
        assert_eq!(params["ops"][1]["path"], json!(["model"]));
    }
}
