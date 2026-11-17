//! DeepSeek configuration with a masked key field, plus historical usage display helpers.

use chrono::{DateTime, Utc};
use gpui::{AnyElement, Context, Entity, Focusable, Hsla, Render, SharedString, Subscription, Task, Window, div, prelude::*, px};
use zeron_proto::{AgentAccount, AgentAccountsSnapshot, CLOUD_ACCOUNTS_DEVICE, HarnessId};
use zeron_rpc::methods;
use crate::{composer::{ComposerInput, ComposerInputEvent}, popover, settings::widgets, state::AppState, theme::Theme};

pub const USAGE_WARN_FRACTION: f32 = 0.80;
pub const USAGE_CRITICAL_FRACTION: f32 = 0.95;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLevel {
    /// < 80% — indigo.
    Normal,
    /// ≥ 80% — amber.
    Warn,
    /// ≥ 95% — red.
    Critical,
}

/// Threshold classification of a usage fraction. Pure.
pub fn usage_level(fraction: f32) -> UsageLevel {
    if fraction >= USAGE_CRITICAL_FRACTION {
        UsageLevel::Critical
    } else if fraction >= USAGE_WARN_FRACTION {
        UsageLevel::Warn
    } else {
        UsageLevel::Normal
    }
}

/// Usage meter columns: window label, reset time, and the meters' overall
/// measure — long enough to read at a glance, short of a full-width gauge.
/// Compact account list geometry: every row shares one usage column and one
/// action slot, so meters and buttons line up down the list however many
/// accounts there are.
const USAGE_LABEL_WIDTH: f32 = 52.0;
const USAGE_BAR_WIDTH: f32 = 88.0;
const USAGE_PERCENT_WIDTH: f32 = 34.0;

pub fn usage_color(level: UsageLevel, theme: &Theme) -> Hsla {
    match level {
        UsageLevel::Normal => theme.accent,
        UsageLevel::Warn => theme.warning,
        UsageLevel::Critical => theme.danger,
    }
}

/// Why a `ListAgentAccounts` load is happening. Pure input to
/// [`force_usage_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadTrigger {
    /// Page construction — the visit's first list.
    Mount,
    /// "Click to retry" after a failed load — still the visit's first
    /// successful list.
    Retry,
    /// The explicit Refresh button.
    Refresh,
    /// After a completed add-account login flow.
    PostLogin,
    /// After Switch/Forget succeeds.
    PostAction,
}

/// Whether a load should ask the engine to probe usage (`forceUsage`). The
/// engine only hits the provider when forced; non-forced lists serve the 60s
/// usage cache or nothing (engine/src/agent_accounts.rs module docs — the
/// design expects the UI to force "on page mount/refresh"). The visit's first
/// list (mount, or retry after a failure) must force, or every first open
/// renders "Usage unavailable" until a manual Refresh — the old app fetched
/// usage on every list. Post-Switch/Forget lists ride the still-warm cache.
pub fn force_usage_for(trigger: LoadTrigger) -> bool {
    match trigger {
        LoadTrigger::Mount | LoadTrigger::Retry | LoadTrigger::Refresh | LoadTrigger::PostLogin => {
            true
        }
        LoadTrigger::PostAction => false,
    }
}

/// Whether a login URL may be opened. Remote logins relay their URL from
/// another device (or from a CLI's output there), so only web sign-in pages
/// and loopback callbacks pass — never `file://` or custom schemes. Pure.
pub fn is_openable_login_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    match scheme.to_ascii_lowercase().as_str() {
        "https" => true,
        "http" => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
            let authority = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            let host = match authority.strip_prefix('[') {
                Some(v6) => v6.split(']').next().unwrap_or(""),
                None => authority.split(':').next().unwrap_or(""),
            };
            matches!(host, "localhost" | "127.0.0.1" | "::1")
        }
        _ => false,
    }
}

pub(crate) fn open_login_url(url: &str, cx: &mut gpui::App) {
    if is_openable_login_url(url) {
        cx.open_url(url);
    } else {
        tracing::warn!("refused to open a non-web sign-in URL");
    }
}

/// Compact absolute reset moment (zeron settings.agents.tsx `formatReset`):
/// a local clock time ("3:45 PM") when it lands within ~22h, a short weekday
/// ("Mon") within a week, else month + day ("Sep 14") — a weekday is noise
/// when the window is a Codex free-tier MONTHLY reset weeks out. The caller
/// prefixes "resets ". Pure given `now`.
pub fn format_reset(resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<String> {
    use chrono::Local;
    let at = resets_at?;
    let local = at.with_timezone(&Local);
    Some(if at.signed_duration_since(now).num_hours() < 22 {
        format!("resets {}", local.format("%-I:%M %p"))
    } else if at.signed_duration_since(now).num_hours() < 24 * 7 {
        format!("resets {}", local.format("%a"))
    } else {
        format!("resets {}", local.format("%b %-d"))
    })
}

/// "Session: resets 3:45 PM · Week: resets Thu" — the usage meters' tooltip.
pub(crate) fn usage_resets(
    windows: &[zeron_proto::AgentUsageWindow],
    now: DateTime<Utc>,
) -> String {
    windows
        .iter()
        .map(|window| match format_reset(window.resets_at, now) {
            Some(reset) => format!("{}: {reset}", window.label),
            None => window.label.clone(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

pub use zeron_proto::providers::{provider_name, reports_usage, signs_in};


pub fn provider_accounts(
    snapshot: &AgentAccountsSnapshot,
    harness: HarnessId,
) -> Vec<&AgentAccount> {
    snapshot
        .accounts
        .iter()
        .filter(|a| a.harness == harness)
        .collect()
}

/// The optimistic half of a switch: `account` becomes the live login of its
/// group — its agent, or for agents that keep a login per model provider,
/// that provider — and every other group keeps its own. Pure.
pub fn mark_switched(snapshot: &mut AgentAccountsSnapshot, account: &AgentAccount) {
    for row in snapshot.accounts.iter_mut() {
        if row.harness == account.harness && row.provider == account.provider {
            row.active = row.id == account.id;
        }
    }
}

/// One mini meter line of the usage column: label, a short bar, percent.
/// The reset moment rides the row's tooltip instead of taking a column.
pub(crate) fn render_usage_meter(
    window: &zeron_proto::AgentUsageWindow,
    theme: &Theme,
) -> AnyElement {
    let fraction = window.used_fraction.clamp(0.0, 1.0);
    let level = usage_level(fraction);
    let fill = usage_color(level, theme).opacity(match level {
        UsageLevel::Normal => 0.8,
        _ => 0.9,
    });
    div()
        .h(px(16.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .text_size(crate::typography::ui_rems(11.5))
        .child(
            div()
                .w(px(USAGE_LABEL_WIDTH))
                .flex_none()
                .truncate()
                .text_color(theme.text_muted)
                .child(SharedString::from(window.label.clone())),
        )
        .child(
            div()
                .w(px(USAGE_BAR_WIDTH))
                .flex_none()
                .h(px(4.0))
                .rounded_full()
                .overflow_hidden()
                .bg(theme.wash(0.08))
                .when(fraction > 0.0, |el| {
                    el.child(
                        div()
                            .h_full()
                            // A 1.5% floor keeps tiny non-zero usage
                            // visible (zeron `max(used, 1.5)%`).
                            .w(gpui::relative(fraction.max(0.015)))
                            .rounded_full()
                            .bg(fill),
                    )
                }),
        )
        .child(
            div()
                .w(px(USAGE_PERCENT_WIDTH))
                .flex_none()
                .text_right()
                .text_color(match level {
                    UsageLevel::Normal => theme.text_muted,
                    _ => usage_color(level, theme),
                })
                .child(SharedString::from(format!(
                    "{}%",
                    (fraction * 100.0).round() as u32
                ))),
        )
        .into_any_element()
}


#[derive(Default)]
pub(crate) struct AccountsSnapshotCache(pub(crate) std::collections::HashMap<Option<String>, AgentAccountsSnapshot>);
impl gpui::Global for AccountsSnapshotCache {}

pub struct AccountsPage {
    state: Entity<AppState>,
    target_device: Option<String>,
    harness: HarnessId,
    embedded: bool,
    input: Entity<ComposerInput>,
    editing: bool,
    focus_pending: bool,
    saving: bool,
    message: Option<String>,
    error: Option<String>,
    action_task: Option<Task<()>>,
    _events: Subscription,
}

impl AccountsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self::create(state, None, HarnessId::DeepSeek, false, cx)
    }

    pub fn new_embedded(state: Entity<AppState>, target: Option<String>, harness: HarnessId, cx: &mut Context<Self>) -> Self {
        Self::create(state, target, harness, true, cx)
    }

    fn create(state: Entity<AppState>, target_device: Option<String>, harness: HarnessId, embedded: bool, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| ComposerInput::new("DeepSeek API key", cx).with_single_line().with_masked());
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) { this.save(cx); }
        });
        Self { state, target_device, harness, embedded, input, editing: false, focus_pending: false, saving: false, message: None, error: None, action_task: None, _events: events }
    }

    pub(crate) fn set_target_device(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        self.target_device = target;
        self.editing = false;
        self.saving = false;
        self.message = None;
        self.error = None;
        self.action_task = None;
        self.input.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    pub(crate) fn set_embedded_harness(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        self.harness = harness;
        cx.notify();
    }

    pub(crate) fn has_escape_surface(&self) -> bool { self.editing }

    pub(crate) fn dismiss_on_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.editing { return false; }
        self.editing = false;
        self.input.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
        true
    }

    pub(crate) fn start_first_login(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        self.harness = harness;
        if harness == HarnessId::DeepSeek {
            self.editing = true;
            self.focus_pending = true;
        } else {
            self.error = Some("This historical provider is unsupported. Configure DeepSeek explicitly; its saved data was not changed.".into());
        }
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.editing || self.saving || self.harness != HarnessId::DeepSeek { return; }
        let key = self.input.read(cx).text().trim().to_owned();
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            self.error = Some("Enter a DeepSeek API key without whitespace.".into());
            cx.notify();
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("Connect to the run device before saving a key.".into());
            cx.notify();
            return;
        };
        let cloud = self.target_device.as_deref() == Some(CLOUD_ACCOUNTS_DEVICE);
        let (method, params) = if cloud {
            let device = self.state.read(cx).cloud_status.as_ref().and_then(|s| s.device_id.clone());
            let Some(device) = device else {
                self.error = Some("Turn Cloud on in Settings → Cloud first.".into());
                cx.notify();
                return;
            };
            (methods::VAULT_PUT_API_KEY, serde_json::json!({ "provider": "deepseek-key", "key": key, "authorizedDevices": [device] }))
        } else {
            // The gateway's shared store is the authority a run reads: the
            // official route's env-shaped ref, written through DSH's own
            // credentials controller (routed to the target device).
            let mut params = zeron_engine::dsh_providers::set_credential_params(
                crate::settings::provider_cards::DEEPSEEK_API_KEY_REF,
                &key,
            );
            if let (Some(target), Some(object)) = (&self.target_device, params.as_object_mut()) {
                object.insert("targetDeviceId".into(), serde_json::json!(target));
            }
            (methods::DSH_SET_CREDENTIAL, params)
        };
        self.saving = true;
        self.error = None;
        let target = self.target_device.clone();
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |page, cx| {
                if page.target_device != target { return; }
                page.saving = false;
                match result {
                    Ok(_) => {
                        page.editing = false;
                        page.input.update(cx, |input, cx| input.set_text("", cx));
                        page.message = Some("DeepSeek API key saved. It will be used for the next run.".into());
                        crate::pickers::bump_harness_catalog(cx);
                    }
                    Err(_) => page.error = Some("Couldn't save the DeepSeek API key. Check the run device and try again.".into()),
                }
                cx.notify();
            }).ok();
        }));
        cx.notify();
    }
}

impl Render for AccountsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        if std::mem::take(&mut self.focus_pending) { window.focus(&self.input.focus_handle(cx), cx); }
        let supported = self.harness == HarnessId::DeepSeek;
        let mut body = div().flex().flex_col().gap(px(12.0))
            .text_size(crate::typography::ui_rems(13.0)).text_color(theme.text_muted)
            .child(if supported {
                "Run coding sessions through the official DeepSeek Harness JSON-RPC SDK. Install the pinned SDK runtime on the device that runs your sessions; see the SDK setup guide."
            } else {
                "This historical provider is unsupported. Its sessions and settings are preserved. Choose a DeepSeek model explicitly before sending."
            });
        if supported {
            body = body
                .child("Add a DeepSeek API key, or provide DEEPSEEK_API_KEY securely to the engine process. A key explicitly saved here takes precedence. The SDK uses Zeron's isolated Harness home, not your personal Harness configuration.")
                .child(widgets::ghost_action(&theme).id("deepseek-sdk-guide").cursor_pointer()
                    .on_click(cx.listener(|_, _, _, cx| cx.open_url("https://github.com/deepseek-ai/deepseek-harness")))
                    .child("SDK setup guide"))
                .when(!self.editing, |el| el.child(
                    widgets::action_button(&theme, widgets::ActionTone::Quiet).id("deepseek-add-key")
                        .on_click(cx.listener(|page, _, _, cx| page.start_first_login(HarnessId::DeepSeek, cx)))
                        .child("Add DeepSeek API key")
                ))
                .when(self.editing, |el| el
                    .child(popover::dialog_field(self.input.clone().into_any_element()))
                    .child(widgets::action_button(&theme, widgets::ActionTone::Solid).id("deepseek-save-key")
                        .when(!self.saving, |el| el.on_click(cx.listener(|page, _, _, cx| page.save(cx))))
                        .child(if self.saving { "Saving…" } else { "Save key" }))
                );
        }
        body = body.when_some(self.error.clone(), |el, error| el.child(widgets::error_strip(&theme, error)))
            .when_some(self.message.clone(), |el, message| el.child(message));
        if self.embedded { return body.into_any_element(); }
        div().id("deepseek-accounts").size_full().overflow_y_scroll()
            .child(widgets::page_column()
                .child(widgets::page_header(&theme, "DeepSeek", None))
                .child(widgets::section_card(&theme).p(px(20.0)).child(body)))
            .into_any_element()
    }
}
