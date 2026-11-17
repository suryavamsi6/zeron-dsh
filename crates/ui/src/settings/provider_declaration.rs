//! The declaration form for a provider route DSH's catalog does not describe.
//!
//! A route pi-ai ships needs no declaration: the catalog names its endpoint,
//! protocol and models. An endpoint the catalog predates — a gateway, a
//! self-hosted server, a provider newer than the installed pi-ai — is declared
//! by hand at `providers.<route>` inside the `llm-pi-ai` namespace, the same
//! address and the same single `set` op DSH's own Models page writes. This form
//! is that write: the route id, its label, the endpoint, the one protocol every
//! model on it speaks, how it authenticates, its static headers, and its model
//! rows. `zeron_engine::dsh_providers` owns the shapes and the validation; the
//! key travels separately through `credentials/set` under the reference the
//! profile records.
//!
//! Two fields exist because a hand-declared route loses what the catalog
//! supplied: the DeepSeek wire switches (pi-ai's `detectCompat` otherwise
//! rewrites the role, the token-cap field and the reasoning block) and the
//! static headers (the catalog path adds its own session header; a declared
//! route has none).

use gpui::{AnyElement, Context, Entity, EventEmitter, Render, Task, Window, div, prelude::*, px};
use zeron_engine::dsh_providers::{self, DeclaredModel, DeclaredRoute};
use zeron_rpc::methods;

use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    popover,
    settings::widgets::{self, ActionTone},
    state::AppState,
    theme::{Theme, hairline},
};

/// What the form tells its owner once a route exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderDeclarationEvent {
    /// A route was written (and its key, when one was typed): the catalog and
    /// the settings store are stale.
    Declared,
}

/// One model row's editable text.
struct ModelDraft {
    id: Entity<ComposerInput>,
    name: Entity<ComposerInput>,
    context: Entity<ComposerInput>,
    max: Entity<ComposerInput>,
    vision: bool,
}

/// One static header row.
struct HeaderDraft {
    name: Entity<ComposerInput>,
    value: Entity<ComposerInput>,
}

/// The declaration form on the Providers settings page.
pub struct ProviderDeclaration {
    state: Entity<AppState>,
    target_device: Option<String>,
    open: bool,
    route: Entity<ComposerInput>,
    display: Entity<ComposerInput>,
    endpoint: Entity<ComposerInput>,
    key: Entity<ComposerInput>,
    needs_key: bool,
    protocols: Vec<String>,
    protocol: String,
    deepseek_compat: bool,
    models: Vec<ModelDraft>,
    headers: Vec<HeaderDraft>,
    /// Route ids already on the device, catalog and declared alike.
    taken: Vec<String>,
    /// The `llm-pi-ai` revision this draft opened at, so a route another window
    /// declared meanwhile is a conflict rather than a silent overwrite.
    revision: Option<u64>,
    busy: Option<&'static str>,
    /// The last outcome: whether it succeeded and the sentence to show.
    result: Option<(bool, String)>,
    task: Option<Task<()>>,
    _events: Vec<gpui::Subscription>,
}

impl EventEmitter<ProviderDeclarationEvent> for ProviderDeclaration {}

fn field(placeholder: &str, cx: &mut Context<ProviderDeclaration>) -> Entity<ComposerInput> {
    cx.new(|cx| ComposerInput::new(placeholder.to_string(), cx).with_single_line())
}

fn secret(placeholder: &str, cx: &mut Context<ProviderDeclaration>) -> Entity<ComposerInput> {
    cx.new(|cx| {
        ComposerInput::new(placeholder.to_string(), cx)
            .with_single_line()
            .with_masked()
    })
}

impl ProviderDeclaration {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut form = Self {
            state,
            target_device: None,
            open: false,
            route: field("acme-gateway", cx),
            display: field("Acme Gateway", cx),
            endpoint: field("https://gateway.acme.test/openai/v1", cx),
            key: secret("API key", cx),
            needs_key: true,
            protocols: dsh_providers::protocols()
                .iter()
                .map(|api| api.to_string())
                .collect(),
            protocol: dsh_providers::protocols()[0].to_string(),
            deepseek_compat: true,
            models: Vec::new(),
            headers: Vec::new(),
            taken: Vec::new(),
            revision: None,
            busy: None,
            result: None,
            task: None,
            _events: Vec::new(),
        };
        form.add_model(cx);
        // Enter in any field submits, the way a dialog's single-line fields do.
        for input in [
            form.route.clone(),
            form.display.clone(),
            form.endpoint.clone(),
        ] {
            let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) && this.open {
                    this.submit(cx);
                }
            });
            form._events.push(events);
        }
        form
    }

    pub(crate) fn set_target_device(&mut self, target: Option<String>) {
        self.target_device = target;
        self.result = None;
    }

    /// Offer exactly the protocols the adapter's schema declares, and refuse to
    /// shadow a route the device already has.
    pub(crate) fn set_catalog(&mut self, protocols: Vec<String>, taken: Vec<String>, revision: Option<u64>) {
        if !protocols.iter().any(|api| api == &self.protocol) {
            self.protocol = protocols.first().cloned().unwrap_or_default();
        }
        self.protocols = protocols;
        self.taken = taken;
        self.revision = revision;
    }

    pub(crate) fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        self.result = None;
        cx.notify();
    }

    fn add_model(&mut self, cx: &mut Context<Self>) {
        self.models.push(ModelDraft {
            id: field("model-id", cx),
            name: field("Display name", cx),
            context: field("1M", cx),
            max: field("64K", cx),
            vision: false,
        });
    }

    fn remove_model(&mut self, index: usize) {
        if self.models.len() > 1 {
            self.models.remove(index);
        }
    }

    fn add_header(&mut self, cx: &mut Context<Self>) {
        self.headers.push(HeaderDraft {
            name: field("x-session-id", cx),
            value: field("value", cx),
        });
    }

    fn remove_header(&mut self, index: usize) {
        self.headers.remove(index);
    }

    /// Collect the form into the declaration the engine validates. A capacity
    /// field that cannot be read is reported against its own row, because the
    /// row is where the user can fix it.
    fn draft(&self, cx: &Context<Self>) -> Result<DeclaredRoute, String> {
        let mut models = Vec::with_capacity(self.models.len());
        for (index, row) in self.models.iter().enumerate() {
            let context = dsh_providers::parse_capacity(row.context.read(cx).text())
                .map_err(|_| format!("model {}: the context window is not a token count (1M, 256K, 128000).", index + 1))?;
            let max = dsh_providers::parse_capacity(row.max.read(cx).text())
                .map_err(|_| format!("model {}: the output limit is not a token count (64K, 128000).", index + 1))?;
            models.push(DeclaredModel {
                id: row.id.read(cx).text().trim().to_string(),
                name: Some(row.name.read(cx).text().trim().to_string()).filter(|name| !name.is_empty()),
                context_window: context,
                max_tokens: max,
                vision: row.vision,
            });
        }
        let headers: Vec<(String, String)> = self
            .headers
            .iter()
            .map(|row| {
                (
                    row.name.read(cx).text().trim().to_string(),
                    row.value.read(cx).text().trim().to_string(),
                )
            })
            .filter(|(name, _)| !name.is_empty())
            .collect();
        let route_id = self.route.read(cx).text().trim().to_string();
        Ok(DeclaredRoute {
            display_name: Some(self.display.read(cx).text().trim().to_string())
                .filter(|name| !name.is_empty()),
            api_key_env: self
                .needs_key
                .then(|| dsh_providers::key_ref_for(&route_id)),
            headers,
            models,
            deepseek_compat: self.deepseek_compat,
            id: route_id,
            base_url: self.endpoint.read(cx).text().trim().to_string(),
            api: self.protocol.clone(),
        })
    }

    /// Write the declaration, then its key: the profile first, because a key
    /// stored under a reference no profile names is unreachable.
    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.result = Some((false, "Connect to the run device to declare a route.".into()));
            cx.notify();
            return;
        };
        let route = match self.draft(cx) {
            Ok(route) => route,
            Err(message) => {
                self.result = Some((false, message));
                cx.notify();
                return;
            }
        };
        let params = match dsh_providers::declare_route_params(&route, &self.taken, self.revision) {
            Ok(params) => params,
            Err(error) => {
                self.result = Some((false, error.message()));
                cx.notify();
                return;
            }
        };
        let key_ref = route.key_ref();
        let key = self.needs_key.then(|| self.key.read(cx).text().trim().to_string());
        let key_params = key
            .filter(|key| !key.is_empty())
            .map(|key| self.with_target(dsh_providers::set_credential_params(&key_ref, &key)));
        let params = self.with_target(params);
        let target = self.target_device.clone();
        let route_id = route.id.clone();
        self.busy = Some("Declaring…");
        self.result = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let client = engine.client();
            let written = client.call(methods::DSH_MUTATE_SETTINGS, params).await;
            let outcome = match written {
                Ok(_) => match key_params {
                    Some(key_params) => client
                        .call(methods::DSH_SET_CREDENTIAL, key_params)
                        .await
                        .map(|_| format!("{route_id} declared, key saved as {key_ref}."))
                        .map_err(|error| {
                            format!("{route_id} declared, but the key was not saved: {error}")
                        }),
                    None => Ok(format!(
                        "{route_id} declared; it authenticates through the endpoint's own chain."
                    )),
                },
                Err(error) => Err(format!("Couldn't declare {route_id}: {error}")),
            };
            let _ = this.update(cx, |form, cx| {
                if form.target_device != target {
                    return;
                }
                form.busy = None;
                match outcome {
                    Ok(message) => {
                        form.result = Some((true, message));
                        form.open = false;
                        form.reset(cx);
                        cx.emit(ProviderDeclarationEvent::Declared);
                    }
                    Err(message) => form.result = Some((false, message)),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        for input in [&self.route, &self.display, &self.endpoint, &self.key] {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.models.clear();
        self.headers.clear();
        self.needs_key = true;
        self.deepseek_compat = true;
        self.add_model(cx);
        cx.notify();
    }

    fn with_target(&self, mut value: serde_json::Value) -> serde_json::Value {
        if let (Some(target), Some(object)) = (&self.target_device, value.as_object_mut()) {
            object.insert("targetDeviceId".into(), serde_json::json!(target));
        }
        value
    }

    /// The live message for the route id field, so an invalid id is named as it
    /// is typed rather than after a refused write.
    fn route_hint(&self, cx: &Context<Self>) -> Option<(bool, String)> {
        let id = self.route.read(cx).text().trim().to_string();
        if id.is_empty() {
            return None;
        }
        if !dsh_providers::route_id_valid(&id) {
            return Some((
                false,
                "Lower-case segments joined by dashes, starting with a letter: acme-gateway.".into(),
            ));
        }
        if self.taken.iter().any(|taken| taken == &id) {
            return Some((false, format!("{id} is already a route on this device.")));
        }
        Some((
            true,
            format!("Key saved as {}", dsh_providers::key_ref_for(&id)),
        ))
    }

    fn render_model_row(&mut self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (id, name, context, max, vision) = {
            let row = &self.models[index];
            (
                row.id.clone(),
                row.name.clone(),
                row.context.clone(),
                row.max.clone(),
                row.vision,
            )
        };
        let row_id = format!("declare-model-{index}");
        div()
            .id(row_id.clone())
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(8.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(hairline(0.08))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w_0().child(popover::dialog_field(id.into_any_element())))
                    .child(div().flex_1().min_w_0().child(popover::dialog_field(name.into_any_element()))),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::field_label(theme, "Context window"))
                            .child(popover::dialog_field(context.into_any_element())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::field_label(theme, "Output limit"))
                            .child(popover::dialog_field(max.into_any_element())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(widgets::field_label(theme, "Vision"))
                            .child(
                                widgets::toggle_switch(theme, vision, format!("declare-vision-{index}"))
                                    .id(format!("declare-vision-toggle-{index}"))
                                    .tab_index(0)
                                    .role(gpui::Role::Switch)
                                    .aria_label("This model takes images")
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |form, _, _, cx| {
                                        if let Some(row) = form.models.get_mut(index) {
                                            row.vision = !row.vision;
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        widgets::ghost_action(theme)
                            .id(format!("declare-model-remove-{index}"))
                            .on_click(cx.listener(move |form, _, _, cx| {
                                form.remove_model(index);
                                cx.notify();
                            }))
                            .child("Remove"),
                    ),
            )
            .into_any_element()
    }

    fn render_open(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let hint = self.route_hint(cx);
        let protocol = self.protocol.clone();
        let protocols = self.protocols.clone();
        let compat_on = self.deepseek_compat;
        let compat_applies = protocol == "openai-completions";
        let needs_key = self.needs_key;

        let mut chips = div().flex().flex_row().flex_wrap().gap(px(6.0));
        for (index, api) in protocols.iter().enumerate() {
            let selected = api == &protocol;
            let label = if selected { format!("{api} ✓") } else { api.clone() };
            let chosen = api.clone();
            let button = widgets::ghost_action(theme)
                .id(format!("declare-protocol-{index}"))
                .tab_index(0)
                .role(gpui::Role::Button)
                .aria_label(format!("Protocol {api}"))
                .when(selected, |el| el.border_1().border_color(theme.accent))
                .on_click(cx.listener(move |form, _, _, cx| {
                    form.protocol = chosen.clone();
                    cx.notify();
                }))
                .child(label);
            chips = chips.child(button);
        }

        let mut models = div().flex().flex_col().gap(px(8.0));
        for index in 0..self.models.len() {
            models = models.child(self.render_model_row(index, theme, cx));
        }

        let mut headers = div().flex().flex_col().gap(px(6.0));
        for index in 0..self.headers.len() {
            let (name, value) = {
                let row = &self.headers[index];
                (row.name.clone(), row.value.clone())
            };
            headers = headers.child(
                div()
                    .id(format!("declare-header-{index}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w_0().child(popover::dialog_field(name.into_any_element())))
                    .child(div().flex_1().min_w_0().child(popover::dialog_field(value.into_any_element())))
                    .child(
                        widgets::ghost_action(theme)
                            .id(format!("declare-header-remove-{index}"))
                            .on_click(cx.listener(move |form, _, _, cx| {
                                form.remove_header(index);
                                cx.notify();
                            }))
                            .child("Remove"),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(widgets::field_label(theme, "Route id"))
                    .child(popover::dialog_field(self.route.clone().into_any_element()))
                    .when_some(hint, |el, (ok, message)| {
                        el.child(
                            div()
                                .text_size(crate::typography::ui_rems(11.5))
                                .text_color(if ok { theme.text_muted } else { theme.danger })
                                .child(message),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(widgets::field_label(theme, "Display name"))
                    .child(popover::dialog_field(self.display.clone().into_any_element())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(widgets::field_label(theme, "Endpoint"))
                    .child(popover::dialog_field(self.endpoint.clone().into_any_element()))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.text_muted)
                            .child("The prefix up to the API path; the protocol appends its own (…/v1 for openai-completions)."),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(widgets::field_label(theme, "Protocol"))
                    .child(chips)
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.text_muted)
                            .child("One protocol serves every model on the route. The list comes from DSH's own config schema."),
                    ),
            )
            .child(
                widgets::card_row(theme, false)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(theme, "Endpoint needs an API key"))
                            .child(widgets::meta_line(
                                theme,
                                vec![
                                    div()
                                        .child("The key is stored in Zeron's shared credential store under the route's own reference.")
                                        .into_any_element(),
                                ],
                            )),
                    )
                    .child(
                        widgets::toggle_switch(theme, needs_key, "declare-needs-key")
                            .id("declare-needs-key-toggle")
                            .tab_index(0)
                            .role(gpui::Role::Switch)
                            .aria_label("The endpoint needs an API key")
                            .cursor_pointer()
                            .on_click(cx.listener(|form, _, _, cx| {
                                form.needs_key = !form.needs_key;
                                cx.notify();
                            })),
                    ),
            )
            .when(needs_key, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(widgets::field_label(theme, "API key"))
                        .child(popover::dialog_field(self.key.clone().into_any_element())),
                )
            })
            .child(
                widgets::card_row(theme, false)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(theme, "DeepSeek-compatible wire switches"))
                            .child(widgets::meta_line(
                                theme,
                                vec![
                                    div()
                                        .child("Keeps max_tokens, the system role and the thinking block as a DeepSeek-compatible endpoint expects. Without them pi-ai's own fallbacks silently rewrite all three.")
                                        .into_any_element(),
                                ],
                            )),
                    )
                    .child(
                        widgets::toggle_switch(theme, compat_on && compat_applies, "declare-deepseek-compat")
                            .id("declare-deepseek-compat-toggle")
                            .tab_index(0)
                            .role(gpui::Role::Switch)
                            .aria_label("Write the DeepSeek-compatible wire switches")
                            .when(!compat_applies, |el| el.opacity(0.4))
                            .on_click(cx.listener(|form, _, _, cx| {
                                form.deepseek_compat = !form.deepseek_compat;
                                cx.notify();
                            })),
                    ),
            )
            .child(widgets::details_label(theme, "Models"))
            .child(models)
            .child(
                widgets::ghost_action(theme)
                    .id("declare-add-model")
                    .on_click(cx.listener(|form, _, _, cx| {
                        form.add_model(cx);
                        cx.notify();
                    }))
                    .child("Add model"),
            )
            .child(widgets::details_label(theme, "Static headers"))
            .child(headers)
            .child(
                widgets::ghost_action(theme)
                    .id("declare-add-header")
                    .on_click(cx.listener(|form, _, _, cx| {
                        form.add_header(cx);
                        cx.notify();
                    }))
                    .child("Add header"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        widgets::action_button(theme, ActionTone::Solid)
                            .id("declare-submit")
                            .on_click(cx.listener(|form, _, _, cx| form.submit(cx)))
                            .child("Declare route"),
                    )
                    .child(
                        widgets::ghost_action(theme)
                            .id("declare-cancel")
                            .on_click(cx.listener(|form, _, _, cx| form.toggle(cx)))
                            .child("Cancel"),
                    )
                    .when_some(self.busy, |el, busy| {
                        el.child(
                            div()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .child(busy),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl Render for ProviderDeclaration {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let result = self.result.clone();
        let open = self.open;
        let mut body = widgets::section_card(&theme).child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(widgets::row_title(&theme, "Declare a route"))
                .child(widgets::meta_line(
                    &theme,
                    vec![
                        div()
                            .child(
                                "For an endpoint DSH's catalog does not describe. The declaration is written \
                                 where DSH reads it (providers.<route> in llm-pi-ai), so the next run uses it \
                                 with no second configuration.",
                            )
                            .into_any_element(),
                    ],
                )),
        );
        if open {
            body = body.child(self.render_open(&theme, cx));
        } else {
            body = body.child(
                widgets::action_button(&theme, ActionTone::Quiet)
                    .id("declare-route-open")
                    .on_click(cx.listener(|form, _, _, cx| form.toggle(cx)))
                    .child("Declare a route"),
            );
        }
        if let Some((ok, message)) = result {
            body = body.child(if ok {
                widgets::meta_line(
                    &theme,
                    vec![
                        div()
                            .text_color(theme.text_muted)
                            .child(message)
                            .into_any_element(),
                    ],
                )
            } else {
                widgets::error_strip(&theme, message)
            });
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_form(cx: &mut gpui::TestAppContext, dir: &std::path::Path) -> gpui::WindowHandle<ProviderDeclaration> {
        use gpui::AppContext;
        cx.update(|cx| {
            crate::settings::init(Default::default(), dir, cx);
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            ProviderDeclaration::new(state, cx)
        })
    }

    #[gpui::test]
    fn the_form_builds_the_declaration_the_engine_writes(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = open_form(cx, dir.path());
        window
            .update(cx, |form, _, cx| {
                form.set_catalog(
                    vec!["openai-completions".into(), "anthropic-messages".into()],
                    vec!["opencode-go".into()],
                    Some(3),
                );
                for (input, text) in [
                    (&form.route, "acme-gateway"),
                    (&form.display, "Acme Gateway"),
                    (&form.endpoint, "https://gateway.acme.test/openai/v1"),
                    (&form.key, "sk-acme"),
                ] {
                    input.update(cx, |input, cx| input.set_text(text, cx));
                }
                let id = form.models[0].id.clone();
                id.update(cx, |input, cx| input.set_text("acme-flash", cx));
                let context = form.models[0].context.clone();
                context.update(cx, |input, cx| input.set_text("1M", cx));
                let max = form.models[0].max.clone();
                max.update(cx, |input, cx| input.set_text("64K", cx));
                form.models[0].vision = true;

                let route = form.draft(cx).expect("a filled form drafts a route");
                assert_eq!(route.id, "acme-gateway");
                assert_eq!(route.api, "openai-completions");
                assert_eq!(route.key_ref(), "ACME_GATEWAY_API_KEY");
                assert_eq!(route.models[0].context_window, Some(1_000_000));
                assert_eq!(route.models[0].max_tokens, Some(64_000));
                assert!(route.models[0].vision);

                let params = dsh_providers::declare_route_params(&route, &form.taken, form.revision)
                    .expect("the engine accepts what the form drafted");
                assert_eq!(params["ns"], "llm-pi-ai");
                assert_eq!(params["expectedRevision"], 3);
                assert_eq!(params["ops"][0]["path"], serde_json::json!(["providers", "acme-gateway"]));
                assert_eq!(
                    params["ops"][0]["value"]["models"][0],
                    serde_json::json!({
                        "id": "acme-flash",
                        "contextWindow": 1_000_000,
                        "maxTokens": 64_000,
                        "input": ["text", "image"],
                    })
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn an_unreadable_capacity_is_named_by_its_row(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = open_form(cx, dir.path());
        window
            .update(cx, |form, _, cx| {
                for (input, text) in [
                    (&form.route, "acme-gateway"),
                    (&form.endpoint, "https://gateway.acme.test/v1"),
                ] {
                    input.update(cx, |input, cx| input.set_text(text, cx));
                }
                let id = form.models[0].id.clone();
                id.update(cx, |input, cx| input.set_text("acme-flash", cx));
                let context = form.models[0].context.clone();
                context.update(cx, |input, cx| input.set_text("lots", cx));
                let error = form.draft(cx).unwrap_err();
                assert!(error.starts_with("model 1:"), "{error}");
                assert!(error.contains("context window"), "{error}");

                let context = form.models[0].context.clone();
                context.update(cx, |input, cx| input.set_text("1M", cx));
                form.toggle(cx);
                assert!(form.open, "the form opens");
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_route_hint_names_a_taken_route_and_shows_the_key_reference(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let window = open_form(cx, dir.path());
        window
            .update(cx, |form, _, cx| {
                form.set_catalog(vec!["openai-completions".into()], vec!["opencode-go".into()], None);
                assert!(form.route_hint(cx).is_none(), "a blank id says nothing yet");

                form.route.update(cx, |input, cx| input.set_text("acme-gateway", cx));
                let (ok, message) = form.route_hint(cx).unwrap();
                assert!(ok);
                assert!(message.contains("ACME_GATEWAY_API_KEY"), "{message}");

                form.route.update(cx, |input, cx| input.set_text("opencode-go", cx));
                let (ok, message) = form.route_hint(cx).unwrap();
                assert!(!ok && message.contains("already a route"), "{message}");

                form.route.update(cx, |input, cx| input.set_text("Acme", cx));
                let (ok, message) = form.route_hint(cx).unwrap();
                assert!(!ok && message.contains("Lower-case"), "{message}");
            })
            .unwrap();
    }
}
