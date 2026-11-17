//! DSH's token meter, read only, beside the provider cards.
//!
//! The meter keeps no aggregate and exposes no usage endpoint: its numbers are
//! session projections, so this panel reads the sessions DSH knows
//! (`session/list`) and the exact projections of the newest few
//! (`session/projections`). Nothing here writes, and nothing contacts a
//! provider: both calls are local reads of DSH's own log, so refreshing cannot
//! bill. A total is a sum over the sessions listed, which the panel says rather
//! than implying DSH reports one.

use gpui::{AnyElement, Context, Entity, Render, Task, Window, div, prelude::*, px};
use zeron_engine::dsh_usage::{self, SessionUsage};
use zeron_rpc::methods;

use crate::{
    popover::Loadable,
    settings::widgets::{self, ActionTone},
    state::AppState,
    theme::Theme,
};

/// How many of the newest sessions get an exact projection read. The list
/// summary already carries the meter's totals; the exact read is what proves
/// them against the log, so it is spent on the rows a person is looking at.
const EXACT_READS: usize = 5;

/// The read-only token meter panel.
pub struct ProviderUsage {
    state: Entity<AppState>,
    target_device: Option<String>,
    sessions: Loadable<Vec<SessionUsage>>,
    /// Whether every row came from an exact projection read.
    exact: bool,
    load_task: Option<Task<()>>,
}

impl ProviderUsage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            state,
            target_device: None,
            sessions: Loadable::Idle,
            exact: false,
            load_task: None,
        };
        panel.load(cx);
        panel
    }

    pub(crate) fn set_target_device(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if self.target_device == target {
            return;
        }
        self.target_device = target;
        self.load(cx);
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
    }

    fn with_target(&self, mut value: serde_json::Value) -> serde_json::Value {
        if let (Some(target), Some(object)) = (&self.target_device, value.as_object_mut()) {
            object.insert("targetDeviceId".into(), serde_json::json!(target));
        }
        value
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sessions = Loadable::Error("Connect to the run device to read its token meter.".into());
            cx.notify();
            return;
        };
        self.sessions = Loadable::Loading;
        let params = self.with_target(dsh_usage::session_list_params());
        let target = self.target_device.clone();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let client = engine.client();
            let listed = client.call(methods::DSH_SESSIONS, params).await;
            let (mut sessions, mut exact) = match listed {
                Ok(value) => (dsh_usage::parse_sessions(&value), false),
                Err(error) => {
                    let _ = this.update(cx, |panel, cx| {
                        if panel.target_device == target {
                            panel.sessions = Loadable::Error(error.to_string());
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at.unwrap_or(0)));
            sessions.truncate(EXACT_READS * 4);
            let wanted: Vec<String> = sessions
                .iter()
                .take(EXACT_READS)
                .map(|session| session.session_id.clone())
                .collect();
            if !wanted.is_empty() {
                exact = true;
                for id in wanted {
                    let mut params = dsh_usage::projections_params(&id);
                    if let (Some(target), Some(object)) = (&target, params.as_object_mut()) {
                        object.insert("targetDeviceId".into(), serde_json::json!(target));
                    }
                    let Ok(value) = client.call(methods::DSH_PROJECTIONS, params).await else {
                        continue;
                    };
                    let Some((totals, context, from_summary)) = dsh_usage::parse_projections(&value)
                    else {
                        continue;
                    };
                    exact &= !from_summary;
                    if let Some(session) = sessions.iter_mut().find(|session| session.session_id == id) {
                        session.totals = totals;
                        session.context = context;
                        session.from_summary = from_summary;
                    }
                }
            }
            let _ = this.update(cx, |panel, cx| {
                if panel.target_device != target {
                    return;
                }
                panel.exact = exact;
                panel.sessions = Loadable::Ready(sessions);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Seed one session for the visual-QA fixture: the meter never answers
    /// there, so the panel shows what a real read renders.
    #[cfg(any(test, feature = "appshots-fixture"))]
    pub fn seed_fixture(&mut self, cx: &mut Context<Self>) {
        self.exact = true;
        self.sessions = Loadable::Ready(vec![SessionUsage {
            session_id: "ses_fixture_0001".into(),
            updated_at: Some(1_700_000_000_000),
            totals: dsh_usage::TokenTotals {
                uncached_input: 412_000,
                output: 18_400,
                cache_read: 1_950_000,
                cache_write: 42_000,
            },
            context: dsh_usage::ContextPressure {
                pressure_tokens: Some(186_000),
                projected_tokens: Some(188_500),
                context_window: Some(1_000_000),
            },
            from_summary: false,
        }]);
        cx.notify();
    }

    fn render_rows(&self, theme: &Theme) -> Vec<AnyElement> {
        let Loadable::Ready(sessions) = &self.sessions else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        let mut total = 0u64;
        for session in sessions {
            total += session.totals.total();
            let short: String = session.session_id.chars().take(14).collect();
            let detail = dsh_usage::usage_summary(session)
                .unwrap_or_else(|| "no usage recorded yet (a blank or unrun session)".into());
            rows.push(
                div()
                    .id(format!("usage-row-{}", session.session_id))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .py(px(4.0))
                    .child(
                        div()
                            .w(px(150.0))
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .child(short),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(detail),
                    )
                    .into_any_element(),
            );
        }
        if sessions.len() > 1 {
            rows.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .py(px(4.0))
                    .child(
                        div()
                            .w(px(150.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .child("all listed"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(format!("{} tokens", dsh_usage::token_label(total))),
                    )
                    .into_any_element(),
            );
        }
        rows
    }
}

impl Render for ProviderUsage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let mut card = widgets::section_card(&theme)
            .child(widgets::row_title(&theme, "Token meter"))
            .child(widgets::meta_line(
                &theme,
                vec![
                    div()
                        .child(
                            "DSH's own accounting, read from its session projections. Read-only, and \
                             local: refreshing never contacts a provider. DSH keeps no cross-session \
                             total, so the sum below covers the sessions it listed.",
                        )
                        .into_any_element(),
                ],
            ));
        match &self.sessions {
            Loadable::Idle | Loadable::Loading => {
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(crate::loaders::ring_spinner(
                            "usage-loading",
                            12.0,
                            theme.text_muted,
                        ))
                        .child("Reading the meter…"),
                );
            }
            Loadable::Error(error) => {
                card = card.child(widgets::error_strip(&theme, error.clone()));
            }
            Loadable::Ready(sessions) => {
                if sessions.is_empty() {
                    card = card.child(
                        div()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(
                                "No sessions in DSH's store yet. The meter records usage as sessions run.",
                            ),
                    );
                } else {
                    card = card.children(self.render_rows(&theme)).child(
                        div()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.text_muted)
                            .child(if self.exact {
                                "Totals are exact projection reads at each session's current log cut."
                            } else {
                                "Totals are the list summaries DSH keeps; an exact read may report cache hints instead."
                            }),
                    );
                }
            }
        }
        card.child(
            widgets::action_button(&theme, ActionTone::Quiet)
                .id("usage-refresh")
                .on_click(cx.listener(|panel, _, _, cx| panel.refresh(cx)))
                .child("Refresh meter"),
        )
    }
}
