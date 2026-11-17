//! Settings → Plugins: every plugin the engine reports, each with what it is,
//! whether it's on, and the settings its manifest declares, drawn natively.
//!
//! The list is the engine's (`ListPlugins`; `SetPluginOff` turns one on or
//! off), and a plugin's declared settings and the values set for it are the
//! engine's plugin store ([`zeron_engine::plugins::settings`], kept in
//! `<data>/plugins/.storage/<id>.settings.json`, beside the plugin's folder).
//! This page draws the controls and writes through that store: it keeps no
//! settings of its own, and a plugin's folder is read, never written.
//!
//! What a manifest may declare, and how each kind is drawn here:
//!
//! - `switch` — a switch;
//! - `choice` — a dropdown (`"style": "select"`, the default) or a row of
//!   segments (`"segmented"`);
//! - `text`, `number`, `shortcut`, `folder` — a value row that opens a small
//!   editor, checked against the setting before it is kept;
//! - `wallpaper` — the app's own row, for a plugin that replaces
//!   `#new-chat-background`. It is drawn by the build that can run the
//!   plugin's drawing; until then the row would control nothing, so it waits
//!   (the store already knows the kind);
//! - `permission` — the app's own row: Zeron says everything on it.
//!
//! A plugin's settings draw while it is on: off, it draws nothing and reads
//! none of them.
//!
//! Everything is keyboard reachable like the rest of Settings: Tab to a row,
//! Space or Enter to flip a switch, ↑/↓ and Enter in a dropdown, Escape
//! closes what is open.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, SharedString, Stateful, Styled,
    Subscription, Task, Window, div, prelude::*, px,
};
use serde_json::{Value as Json, json};
use zeron_engine::plugins::settings::{PluginSettings, Setting};
use zeron_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons;
use crate::popover;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// The manifest file a plugin folder holds.
pub const MANIFEST_FILE: &str = "zeron-plugin.json";

/// What the engine says of a plugin's state, in the page's words.
pub fn status_label(status: &str) -> &'static str {
    match status {
        "on" => "On",
        "off" => "Off",
        "pending" => "Waiting for you",
        "tampered" => "Changed since you allowed it",
        "broken" => "Broken",
        _ => "Unknown",
    }
}

/// Whether a plugin in this state can be turned on or off here: one waiting
/// for the person's decision, or whose files changed since they allowed it,
/// needs the consent sheet instead.
pub fn can_toggle(status: &str) -> bool {
    matches!(status, "on" | "off")
}

/// Who wrote a plugin, as the engine says it (`author` on `ListPlugins`).
pub fn author_label(author: &Json) -> String {
    match author.get("kind").and_then(|k| k.as_str()) {
        Some("you") => "You".to_string(),
        Some("zeron") => "Zeron".to_string(),
        Some("verified") => match author.get("username").and_then(|u| u.as_str()) {
            Some(name) if author.get("mine").and_then(Json::as_bool) == Some(true) => {
                format!("{name} (your other device)")
            }
            Some(name) => name.to_string(),
            None => "Verified".to_string(),
        },
        _ => "Unsigned".to_string(),
    }
}

/// One option a choice offers, as its control draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// The native control a declared setting is drawn with.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    Switch {
        on: bool,
    },
    Choice {
        options: Vec<ChoiceOption>,
        selected: usize,
        segmented: bool,
    },
    Text {
        value: String,
        placeholder: Option<String>,
    },
    Number {
        value: f64,
        min: Option<f64>,
        max: Option<f64>,
        unit: Option<String>,
    },
    Shortcut {
        value: String,
    },
    Folder {
        value: String,
    },
    /// The app's own row: whether this plugin draws a new chat's background.
    Wallpaper {
        on: bool,
    },
    /// The app's own row: a platform permission, said by the app.
    Permission {
        permission: String,
    },
}

impl Control {
    /// Whether the control holds a value the plugin reads (the app's own
    /// rows don't).
    pub fn is_value(&self) -> bool {
        !matches!(self, Self::Wallpaper { .. } | Self::Permission { .. })
    }
}

/// The control `setting` is drawn with now, from its value (`value`, as the
/// store holds it) and the plugin drawing a new chat's background
/// (`wallpaper`, if any).
pub fn control(setting: &Setting, value: &Json, plugin: &str, wallpaper: Option<&str>) -> Control {
    match setting.kind.as_str() {
        "switch" => Control::Switch {
            on: value.as_bool().unwrap_or(false),
        },
        "choice" => {
            let options: Vec<ChoiceOption> = setting
                .options()
                .iter()
                .map(|option| ChoiceOption {
                    value: option
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    label: option
                        .get("label")
                        .and_then(|l| l.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    description: option
                        .get("description")
                        .and_then(|d| d.as_str())
                        .map(str::to_string),
                })
                .collect();
            let chosen = value.as_str().unwrap_or_default();
            let selected = options
                .iter()
                .position(|option| option.value == chosen)
                .unwrap_or(0);
            Control::Choice {
                options,
                selected,
                segmented: setting.segmented(),
            }
        }
        "text" => Control::Text {
            value: value.as_str().unwrap_or_default().to_string(),
            placeholder: setting
                .json
                .get("placeholder")
                .and_then(|p| p.as_str())
                .map(str::to_string),
        },
        "number" => Control::Number {
            value: value.as_f64().unwrap_or(0.0),
            min: setting.json.get("min").and_then(Json::as_f64),
            max: setting.json.get("max").and_then(Json::as_f64),
            unit: setting
                .json
                .get("unit")
                .and_then(|u| u.as_str())
                .map(str::to_string),
        },
        "shortcut" => Control::Shortcut {
            value: value.as_str().unwrap_or_default().to_string(),
        },
        "folder" => Control::Folder {
            value: value.as_str().unwrap_or_default().to_string(),
        },
        "wallpaper" => Control::Wallpaper {
            on: wallpaper == Some(plugin),
        },
        "permission" => Control::Permission {
            permission: setting
                .json
                .get("permission")
                .and_then(|p| p.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        // (None: the store leaves an unknown kind out of what it declares.)
        _ => Control::Text {
            value: String::new(),
            placeholder: None,
        },
    }
}

/// One plugin, as the page draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginRow {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub builtin: bool,
    /// The engine's state: `on`, `off`, `pending`, `tampered` or `broken`.
    pub status: String,
    pub author: Json,
    pub permissions: Vec<String>,
    pub settings: PluginSettings,
    /// Why its manifest couldn't be read (said under its name).
    pub manifest_error: Option<SharedString>,
}

impl PluginRow {
    pub fn on(&self) -> bool {
        self.status == "on"
    }

    /// The quiet line under the plugin's name: who wrote it, what it is.
    pub fn meta(&self) -> Vec<String> {
        let mut parts = vec![author_label(&self.author)];
        if self.builtin {
            parts.push("Built into Zeron".to_string());
        }
        if !self.permissions.is_empty() {
            parts.push(format!(
                "{} permission{}",
                self.permissions.len(),
                if self.permissions.len() == 1 { "" } else { "s" }
            ));
        }
        parts
    }

    /// One plugin as the engine listed it (`item`), with what its manifest
    /// declares (`manifest`, read from its folder) and the store `storage`.
    pub fn from_item(item: &Json, manifest: Option<&Json>, storage: &Path) -> Self {
        let builtin = item.get("builtin").and_then(Json::as_bool) == Some(true);
        let (settings, manifest_error) = match manifest {
            Some(manifest) => (PluginSettings::read(storage, manifest, builtin), None),
            None => (
                PluginSettings::default(),
                Some(SharedString::from(
                    "Its zeron-plugin.json couldn't be read, so its settings aren't shown.",
                )),
            ),
        };
        Self {
            id: item
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string(),
            name: item
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            version: item
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            description: item
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or_default()
                .to_string(),
            builtin,
            status: item
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("off")
                .to_string(),
            author: item.get("author").cloned().unwrap_or(Json::Null),
            permissions: item
                .get("permissions")
                .and_then(|p| p.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|p| p.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            settings,
            manifest_error,
        }
    }
}

/// An open value editor: one setting of one plugin.
struct Editor {
    plugin: String,
    setting: String,
    label: SharedString,
    input: Entity<ComposerInput>,
    error: Option<SharedString>,
    _events: Subscription,
}

pub struct PluginsPage {
    state: Entity<AppState>,
    data_dir: PathBuf,
    scroll: widgets::PageScroll,
    rows: Vec<PluginRow>,
    loading: bool,
    error: Option<SharedString>,
    /// Which plugin draws a new chat's background now (the app's own
    /// setting, in `ui-settings.json`).
    wallpaper: Option<String>,
    /// The dropdowns, by `plugin:setting`.
    selects: HashMap<String, widgets::SelectState>,
    editor: Option<Editor>,
    /// The plugin whose switch waits on the engine, so it can't be flipped
    /// twice mid-flight.
    busy: Option<String>,
    task: Option<Task<()>>,
}

impl PluginsPage {
    pub fn new(state: Entity<AppState>, data_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let wallpaper = crate::settings::current(cx).plugin_wallpaper;
        let mut page = Self {
            state,
            data_dir,
            scroll: widgets::PageScroll::default(),
            rows: Vec::new(),
            loading: true,
            error: None,
            wallpaper,
            selects: HashMap::new(),
            editor: None,
            busy: None,
            task: None,
        };
        page.reload(cx);
        page
    }

    /// The plugin store's folder: `<data>/plugins/.storage`.
    fn storage(&self) -> PathBuf {
        zeron_engine::plugins::settings::storage_in(&self.data_dir.join("plugins"))
    }

    /// Ask the engine for the list, then read each plugin's manifest on the
    /// background executor so the window never waits on a disk.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.loading = false;
            self.error = Some("The engine isn't connected, so plugins can't be listed.".into());
            cx.notify();
            return;
        };
        self.loading = true;
        self.error = None;
        let storage = self.storage();
        self.task = Some(cx.spawn(async move |this, cx| {
            let reply = engine.client().call(methods::LIST_PLUGINS, json!({})).await;
            let items = match reply {
                Ok(reply) => reply
                    .get("plugins")
                    .and_then(|p| p.as_array())
                    .cloned()
                    .unwrap_or_default(),
                Err(error) => {
                    this.update(cx, |page, cx| {
                        page.loading = false;
                        page.error = Some(format!("Plugins couldn't be listed: {error}").into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let dirs: Vec<(usize, PathBuf)> = items
                .iter()
                .enumerate()
                .filter_map(|(ix, item)| {
                    item.get("dir")
                        .and_then(|d| d.as_str())
                        .map(|dir| (ix, PathBuf::from(dir)))
                })
                .collect();
            let manifests = cx
                .background_spawn(async move {
                    let mut found: HashMap<usize, Json> = HashMap::new();
                    for (ix, dir) in dirs {
                        if let Some(manifest) = read_manifest(&dir) {
                            found.insert(ix, manifest);
                        }
                    }
                    found
                })
                .await;
            let rows: Vec<PluginRow> = items
                .iter()
                .enumerate()
                .map(|(ix, item)| PluginRow::from_item(item, manifests.get(&ix), &storage))
                .collect();
            this.update(cx, |page, cx| {
                page.rows = rows;
                page.loading = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Turn a plugin on or off (`SetPluginOff`), letting go of the wallpaper
    /// when the plugin drawing it is turned off.
    fn set_on(&mut self, id: String, on: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("The engine isn't connected.".into());
            cx.notify();
            return;
        };
        self.busy = Some(id.clone());
        self.task = Some(cx.spawn(async move |this, cx| {
            let reply = engine
                .client()
                .call(
                    methods::SET_PLUGIN_OFF,
                    json!({ "id": id.clone(), "off": !on }),
                )
                .await;
            this.update(cx, |page, cx| {
                page.busy = None;
                match reply {
                    Ok(item) => {
                        if let Some(row) = page.rows.iter_mut().find(|row| row.id == id)
                            && let Some(status) = item.get("status").and_then(|s| s.as_str())
                        {
                            row.status = status.to_string();
                        }
                        if !on && page.wallpaper.as_deref() == Some(id.as_str()) {
                            page.choose_wallpaper(None, cx);
                        }
                    }
                    Err(error) => {
                        page.error =
                            Some(format!("That plugin couldn't be changed: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Keep `id` as the plugin drawing a new chat's background (none: back
    /// to Zeron's own wallpaper).
    fn choose_wallpaper(&mut self, id: Option<String>, cx: &mut App) {
        if self.wallpaper == id {
            return;
        }
        self.wallpaper = id.clone();
        crate::settings::set_plugin_wallpaper(id, cx);
    }

    /// Keep one setting of one plugin: checked and written by the plugin
    /// store, in the plugin folder's own storage.
    fn set_value(&mut self, plugin: &str, setting: &str, value: Json, cx: &mut Context<Self>) {
        let storage = self.storage();
        let Some(row) = self.rows.iter_mut().find(|row| row.id == plugin) else {
            return;
        };
        match row.settings.set(&storage, setting, &value) {
            Ok(_) => self.error = None,
            Err(error) => self.error = Some(error.into()),
        }
        cx.notify();
    }

    fn open_editor(&mut self, plugin: &str, setting: &Setting, cx: &mut Context<Self>) {
        let label: SharedString = SharedString::from(setting.label().to_string());
        let value = self
            .rows
            .iter()
            .find(|row| row.id == plugin)
            .and_then(|row| row.settings.value(&setting.id))
            .unwrap_or(Json::Null);
        let text = match value {
            Json::Number(n) => n.to_string(),
            Json::String(s) => s,
            _ => String::new(),
        };
        let input = cx.new(|cx| ComposerInput::new("Its value", cx));
        input.update(cx, |input, cx| input.set_text(text, cx));
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_editor(cx);
            }
        });
        self.editor = Some(Editor {
            plugin: plugin.to_string(),
            setting: setting.id.clone(),
            label,
            input,
            error: None,
            _events: events,
        });
        cx.notify();
    }

    fn submit_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let text = editor.input.read(cx).text().trim().to_string();
        let (plugin, setting) = (editor.plugin.clone(), editor.setting.clone());
        let declared = self
            .rows
            .iter()
            .find(|row| row.id == plugin)
            .and_then(|row| {
                row.settings
                    .declared
                    .iter()
                    .find(|s| s.id == setting)
                    .cloned()
            });
        let Some(declared) = declared else {
            self.editor = None;
            cx.notify();
            return;
        };
        let value = match declared.kind.as_str() {
            "number" => match text.parse::<f64>() {
                Ok(number) => json!(number),
                Err(_) => {
                    if let Some(editor) = self.editor.as_mut() {
                        editor.error = Some("That isn't a number.".into());
                    }
                    cx.notify();
                    return;
                }
            },
            _ => json!(text),
        };
        let storage = self.storage();
        let result = self
            .rows
            .iter_mut()
            .find(|row| row.id == plugin)
            .map(|row| row.settings.set(&storage, &setting, &value));
        match result {
            Some(Ok(_)) => {
                self.editor = None;
                self.error = None;
            }
            Some(Err(error)) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.error = Some(error.into());
                }
            }
            None => self.editor = None,
        }
        cx.notify();
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    /// Whether a dialog Escape would close is open.
    pub(crate) fn has_escape_surface(&self) -> bool {
        self.editor.is_some()
    }

    pub(crate) fn dismiss_on_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.editor.take().is_none() {
            return false;
        }
        cx.notify();
        true
    }
}

/// A plugin's manifest as written: its settings are declared there, and the
/// engine's listing carries the typed view alone.
pub fn read_manifest(dir: &Path) -> Option<Json> {
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
impl PluginsPage {
    /// A page already holding `rows`, with nothing asking the engine: the
    /// renderer and the store are what its tests put under the pointer.
    pub(crate) fn for_test(
        state: Entity<AppState>,
        data_dir: PathBuf,
        rows: Vec<PluginRow>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            state,
            data_dir,
            scroll: widgets::PageScroll::default(),
            rows,
            loading: false,
            error: None,
            wallpaper: None,
            selects: HashMap::new(),
            editor: None,
            busy: None,
            task: None,
        }
    }
}

impl popover::ScrollRailHost for PluginsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

/// Whether a key flips a switch: Space or Enter, pressed (not held).
fn is_switch_activation(key: &str, is_held: bool) -> bool {
    !is_held && matches!(key, "enter" | "space")
}

/// A switch's activation target: pointer as a click, keyboard as Space or
/// Enter, focus ring on the accent.
fn interactive_row(
    element: Stateful<gpui::Div>,
    accent: gpui::Hsla,
    on_activate: impl Fn(&mut PluginsPage, &mut Window, &mut Context<PluginsPage>) + 'static,
    cx: &mut Context<PluginsPage>,
) -> Stateful<gpui::Div> {
    let on_activate = std::rc::Rc::new(on_activate);
    let click = on_activate.clone();
    let press = on_activate;
    element
        .tab_index(0)
        .focus_visible(move |style| style.border_2().border_color(accent))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| click(this, window, cx)))
        .on_key_down(
            cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                if is_switch_activation(&event.keystroke.key, event.is_held) {
                    cx.stop_propagation();
                    press(this, window, cx);
                }
            }),
        )
}

impl Render for PluginsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let viewport = window.viewport_size();
        let count = self.rows.len();

        let mut column = widgets::page_column()
            .child(widgets::page_header(
                &theme,
                "Plugins",
                (!self.loading).then_some(count),
            ))
            .child(widgets::page_subtitle(
                &theme,
                "Extensions that draw into Zeron and read what it gives them, each with the \
                 settings its own manifest declares.",
            ));

        if let Some(error) = self.error.clone() {
            column = column.child(
                div()
                    .mt(px(16.0))
                    .child(widgets::error_strip(&theme, error)),
            );
        }

        if self.loading {
            column = column.child(div().mt(px(24.0)).child(widgets::meta_line(
                &theme,
                vec![div()
                    .child("Reading the plugins Zeron has…")
                    .into_any_element()],
            )));
        } else if self.rows.is_empty() {
            column = column.child(div().mt(px(24.0)).child(widgets::meta_line(
                &theme,
                vec![div()
                    .child("No plugins are installed. Zeron's own plugins ship with it.")
                    .into_any_element()],
            )));
        } else {
            column = column.child(div().mt(px(24.0)).child(widgets::section_label(
                &theme,
                format!("Installed ({count})"),
            )));
            for ix in 0..self.rows.len() {
                column =
                    column.child(widgets::section_card(&theme).child(self.card(ix, window, cx)));
            }
        }

        let dialog = self.render_editor(viewport, cx);
        let scrollbar = popover::rail(self, "plugins-settings-page-scrollbar", &theme, cx);
        div()
            .id("plugins-settings-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                crate::edge_fade::edge_faded(
                    16.0,
                    true,
                    true,
                    div()
                        .id("plugins-settings-page")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll.scroll)
                        .child(column.child(div().h(px(48.0)))),
                )
                .fade_overflow_y(&self.scroll.scroll),
            )
            .child(widgets::page_reveal(&self.scroll.scroll))
            .children(scrollbar)
            .when_some(dialog, |el, dialog| el.child(dialog))
    }
}

impl PluginsPage {
    /// One plugin's block: what it is, whether it's on, then its settings.
    fn card(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).for_settings_surface();
        let accent = theme.accent;
        let row = self.rows[ix].clone();
        let busy = self.busy.as_deref() == Some(row.id.as_str());
        let toggleable = can_toggle(&row.status);
        let on = row.on();
        let toggle_id = row.id.clone();

        let switch = div()
            .id(format!("plugin-switch-{toggle_id}"))
            .flex_none()
            .w(px(widgets::SWITCH_WIDTH))
            .h(px(40.0))
            .flex()
            .items_center()
            .justify_center()
            .role(gpui::Role::Switch)
            .aria_label(SharedString::from(format!(
                "{}, {}",
                row.name,
                status_label(&row.status)
            )))
            .aria_toggled(if on {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .when(!toggleable, |el| {
                el.aria_description("This plugin waits for your decision in its own sheet")
            })
            .child(widgets::toggle_switch(
                &theme,
                on,
                format!("plugin-toggle-{toggle_id}"),
            ));
        let switch: AnyElement = if toggleable && !busy {
            interactive_row(
                switch,
                accent,
                move |page, _window, cx| page.set_on(toggle_id.clone(), !on, cx),
                cx,
            )
            .into_any_element()
        } else {
            switch.into_any_element()
        };

        let mut meta: Vec<AnyElement> = vec![
            div()
                .child(SharedString::from(status_label(&row.status)))
                .into_any_element(),
        ];
        meta.extend(
            row.meta()
                .into_iter()
                .map(|part| div().child(part).into_any_element()),
        );

        let mut heading = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .child(
                div()
                    .flex_none()
                    .w(px(24.0))
                    .h(px(32.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        icons::icon(if row.builtin {
                            icons::WIDGET
                        } else {
                            icons::MAGIC_STICK_3
                        })
                        .size(px(16.0))
                        .text_color(theme.text_muted),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(widgets::row_title(&theme, row.name.clone()))
                            .child(widgets::badge(&theme, format!("v{}", row.version))),
                    )
                    .child(widgets::meta_line(&theme, meta)),
            )
            .child(switch);
        if !row.description.is_empty() {
            heading = heading.child(
                div()
                    .w_full()
                    .text_size(crate::typography::ui_rems(12.0))
                    .line_height(crate::typography::ui_rems(18.0))
                    .text_color(theme.text_muted)
                    .child(row.description.clone()),
            );
        }

        let mut card = div()
            .flex()
            .flex_col()
            .child(widgets::card_row(&theme, true).child(heading));

        if let Some(error) = row.manifest_error.clone() {
            card = card.child(widgets::card_row(&theme, false).child(widgets::meta_line(
                &theme,
                vec![div().child(error).into_any_element()],
            )));
        }

        // Its settings are its own while it runs: off, it draws nothing and
        // reads none of them, so the page shows none (as zgx does).
        let mut first = false;
        let mut section: Option<String> = None;
        for setting in row.settings.declared.clone().into_iter().filter(|_| row.on()) {
            if setting.kind == "wallpaper" {
                continue;
            }
            let value = row.settings.value(&setting.id).unwrap_or(Json::Null);
            let here = setting.section().map(str::to_string);
            let heading = here != section;
            card = card.child(self.setting_row(&row, &setting, &value, first, heading, window, cx));
            section = here;
            first = false;
        }
        card.into_any_element()
    }

    /// One declared setting, drawn as its kind's native control.
    fn setting_row(
        &mut self,
        row: &PluginRow,
        setting: &Setting,
        value: &Json,
        first: bool,
        heading: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).for_settings_surface();
        let accent = theme.accent;
        let control = control(setting, value, &row.id, self.wallpaper.as_deref());
        let plugin = row.id.clone();
        let setting_id = setting.id.clone();
        let key = format!("{plugin}:{setting_id}");

        let mut left = div()
            .flex_1()
            .min_w(px(160.0))
            .flex()
            .flex_col()
            .child(widgets::row_title(&theme, setting.label().to_string()));
        let mut description = setting
            .json
            .get("description")
            .and_then(|d| d.as_str())
            .map(str::to_string);
        if let Control::Choice {
            options, selected, ..
        } = &control
            && let Some(chosen) = options
                .get(*selected)
                .and_then(|option| option.description.clone())
        {
            description = Some(chosen);
        }
        if let Some(description) = description {
            left = left.child(
                div()
                    .mt(px(Theme::TEXT_STACK_GAP))
                    .text_size(crate::typography::ui_rems(12.0))
                    .line_height(crate::typography::ui_rems(16.0))
                    .text_color(theme.text_muted)
                    .child(description),
            );
        }

        let right: AnyElement = match &control {
            Control::Switch { on } => {
                let on = *on;
                let id = setting_id.clone();
                let selector = format!("plugin-setting-hit-{key}");
                let cell = div()
                    .id(format!("plugin-setting-switch-{key}"))
                    .debug_selector(move || selector)
                    .flex_none()
                    .w(px(widgets::SWITCH_WIDTH))
                    .h(px(40.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .role(gpui::Role::Switch)
                    .aria_label(SharedString::from(setting.label().to_string()))
                    .aria_toggled(if on {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .child(widgets::toggle_switch(
                        &theme,
                        on,
                        format!("plugin-setting-{key}"),
                    ));
                interactive_row(
                    cell,
                    accent,
                    move |page, _window, cx| {
                        page.set_value(&plugin, &id, json!(!on), cx);
                    },
                    cx,
                )
                .into_any_element()
            }
            Control::Choice {
                options,
                selected,
                segmented,
            } => {
                if *segmented {
                    self.segmented(
                        &theme,
                        &key,
                        options,
                        *selected,
                        &plugin,
                        &setting_id,
                        window,
                        cx,
                    )
                } else {
                    let reach_key = key.clone();
                    let chosen = *selected;
                    let options = options.clone();
                    let select = widgets::select(
                        format!("plugin-setting-{key}"),
                        setting.label().to_string(),
                        &theme,
                        move |page: &mut Self| page.selects.entry(reach_key.clone()).or_default(),
                    )
                    .options(
                        options.iter().map(|option| {
                            let mut entry = widgets::SelectOption::new(option.label.clone());
                            if let Some(description) = option.description.clone() {
                                entry = entry.detail(description);
                            }
                            entry
                        }),
                        chosen,
                    )
                    .width(180.0)
                    .on_select(
                        move |page: &mut Self, ix: usize, _, cx: &mut Context<Self>| {
                            let value = page
                                .rows
                                .iter()
                                .find(|row| row.id == plugin)
                                .and_then(|row| {
                                    row.settings.declared.iter().find(|s| s.id == setting_id)
                                })
                                .and_then(|s| s.options().get(ix).cloned());
                            if let Some(option) = value {
                                page.set_value(&plugin, &setting_id, option, cx);
                            }
                        },
                    );
                    let state = self.selects.entry(key.clone()).or_default();
                    select.render(state, cx).into_any_element()
                }
            }
            Control::Text { value, placeholder } => self.value_row(
                &theme,
                &key,
                value.clone(),
                placeholder.clone(),
                row,
                setting,
                cx,
            ),
            Control::Number { value, unit, .. } => {
                let text = match unit.as_deref() {
                    Some(unit) => format!("{value} {unit}"),
                    None => format!("{value}"),
                };
                self.value_row(&theme, &key, text, None, row, setting, cx)
            }
            Control::Shortcut { value } => {
                let text = if value.is_empty() {
                    "None".to_string()
                } else {
                    value.clone()
                };
                self.value_row(&theme, &key, text, None, row, setting, cx)
            }
            Control::Folder { value } => {
                let text = if value.is_empty() {
                    "None".to_string()
                } else {
                    value.clone()
                };
                self.value_row(&theme, &key, text, None, row, setting, cx)
            }
            // (The app's own rows: see the module's note.)
            Control::Wallpaper { .. } | Control::Permission { .. } => div().into_any_element(),
        };

        let row = widgets::card_row(&theme, first).child(left).child(right);
        match setting.section().filter(|_| heading) {
            Some(section) => div()
                .child(
                    div()
                        .mx(px(16.0))
                        .pt(px(14.0))
                        .child(widgets::details_label(&theme, section.to_string())),
                )
                .child(row)
                .into_any_element(),
            None => row.into_any_element(),
        }
    }

    /// A row of segments for a `choice` drawn `segmented`.
    fn segmented(
        &mut self,
        theme: &Theme,
        key: &str,
        options: &[ChoiceOption],
        selected: usize,
        plugin: &str,
        setting: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reduced = cx.reduce_motion();
        let mut row = div().flex().flex_row().flex_wrap().gap(px(2.0));
        for (ix, option) in options.iter().enumerate() {
            let chosen = ix == selected;
            let tab_key = format!("{key}:tab:{ix}");
            let t = widgets::tab_selection_t(window, tab_key.clone(), chosen, reduced);
            let plugin = plugin.to_string();
            let setting = setting.to_string();
            let value = option.value.clone();
            row = row.child(
                widgets::section_tab(
                    theme,
                    chosen,
                    t,
                    SharedString::from(tab_key.clone()),
                    format!("{tab_key}-hover"),
                )
                .cursor_pointer()
                .role(gpui::Role::Tab)
                .aria_label(SharedString::from(option.label.clone()))
                .child(option.label.clone())
                .on_click(cx.listener(move |page, _, _, cx| {
                    page.set_value(&plugin, &setting, json!(value), cx);
                })),
            );
        }
        row.into_any_element()
    }

    /// A value row: what the setting holds now, opening a small editor.
    fn value_row(
        &mut self,
        theme: &Theme,
        key: &str,
        text: String,
        placeholder: Option<String>,
        row: &PluginRow,
        setting: &Setting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = theme.accent;
        let plugin = row.id.clone();
        let setting = setting.clone();
        let muted = text.is_empty() || text == "None";
        let cell = widgets::select_trigger(theme, format!("plugin-value-{key}"), false)
            .w(px(180.0))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_color(if muted { theme.text_muted } else { theme.text })
                    .child(if muted {
                        placeholder.unwrap_or_else(|| "None".to_string())
                    } else {
                        text
                    }),
            )
            .child(widgets::select_chevron(theme, false));
        interactive_row(
            cell,
            accent,
            move |page, _window, cx| page.open_editor(&plugin, &setting, cx),
            cx,
        )
        .into_any_element()
    }

    /// The small editor a value row opens.
    fn render_editor(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.editor.as_ref()?;
        let theme = Theme::of(cx).for_popup();
        let label = editor.label.clone();
        let input = editor.input.clone();
        let error = editor.error.clone();
        let card = popover::dialog_card(&theme)
            .id("plugin-setting-editor")
            .role(gpui::Role::Dialog)
            .aria_label(label.clone())
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.editor = None;
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(popover::dialog_title(&theme, &label))
            .child(
                div()
                    .mt(px(12.0))
                    .child(popover::dialog_field(input.into_any_element())),
            )
            .when_some(error, |card, error| {
                card.child(div().mt(px(8.0)).child(widgets::error_strip(&theme, error)))
            })
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        widgets::text_action(&theme, widgets::ActionTone::Quiet, "Cancel")
                            .id("plugin-editor-cancel")
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editor = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        widgets::text_action(&theme, widgets::ActionTone::Solid, "Save")
                            .id("plugin-editor-save")
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_editor(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal(
            "plugin-setting-editor-dialog",
            viewport,
            card,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_engine::plugins::settings::declared;

    fn manifest() -> Json {
        json!({
            "id": "demo",
            "settings": [
                { "id": "on", "kind": "switch", "label": "On", "default": true },
                { "id": "scene", "kind": "choice", "label": "Scene", "default": "dusk",
                  "options": [
                    { "value": "shuffle", "label": "Shuffle", "description": "Another each time." },
                    { "value": "dusk", "label": "Dusk" },
                  ] },
                { "id": "parallax", "kind": "choice", "label": "Parallax", "style": "segmented",
                  "options": [ { "value": "off", "label": "Off" }, { "value": "deep", "label": "Deep" } ] },
                { "id": "words", "kind": "text", "label": "Words", "placeholder": "Say something" },
                { "id": "every", "kind": "number", "label": "Every", "min": 30, "max": 600, "unit": "s" },
                { "id": "keys", "kind": "shortcut", "label": "Keys" },
                { "id": "out", "kind": "folder", "label": "Save to" },
            ],
        })
    }

    fn setting(declared: &[Setting], id: &str) -> Setting {
        declared.iter().find(|s| s.id == id).cloned().unwrap()
    }

    #[test]
    fn every_value_kind_gets_its_own_native_control() {
        let declared = declared(&manifest(), false);
        let defaults = plugin_settings_values(&declared);
        assert_eq!(
            control(&setting(&declared, "on"), &defaults["on"], "demo", None),
            Control::Switch { on: true }
        );
        assert_eq!(
            control(
                &setting(&declared, "scene"),
                &defaults["scene"],
                "demo",
                None
            ),
            Control::Choice {
                options: vec![
                    ChoiceOption {
                        value: "shuffle".into(),
                        label: "Shuffle".into(),
                        description: Some("Another each time.".into()),
                    },
                    ChoiceOption {
                        value: "dusk".into(),
                        label: "Dusk".into(),
                        description: None,
                    },
                ],
                selected: 1,
                segmented: false,
            }
        );
        // A segmented choice says so, and its selection follows the value.
        let parallax = control(
            &setting(&declared, "parallax"),
            &json!("deep"),
            "demo",
            None,
        );
        assert!(matches!(
            parallax,
            Control::Choice {
                selected: 1,
                segmented: true,
                ..
            }
        ));
        assert_eq!(
            control(&setting(&declared, "words"), &Json::Null, "demo", None),
            Control::Text {
                value: String::new(),
                placeholder: Some("Say something".into()),
            }
        );
        assert_eq!(
            control(&setting(&declared, "every"), &json!(60.0), "demo", None),
            Control::Number {
                value: 60.0,
                min: Some(30.0),
                max: Some(600.0),
                unit: Some("s".into()),
            }
        );
        assert_eq!(
            control(
                &setting(&declared, "keys"),
                &json!("ctrl+alt+g"),
                "demo",
                None
            ),
            Control::Shortcut {
                value: "ctrl+alt+g".into()
            }
        );
        assert_eq!(
            control(&setting(&declared, "out"), &json!("/tmp"), "demo", None),
            Control::Folder {
                value: "/tmp".into()
            }
        );
    }

    #[test]
    fn a_choice_pointing_at_no_option_shows_the_first() {
        let declared = declared(&manifest(), false);
        let ctl = control(&setting(&declared, "scene"), &json!("gone"), "demo", None);
        assert!(matches!(ctl, Control::Choice { selected: 0, .. }));
    }

    /// The store's values for `declared` with nothing set (its defaults).
    fn plugin_settings_values(declared: &[Setting]) -> serde_json::Map<String, Json> {
        zeron_engine::plugins::settings::values(declared, &serde_json::Map::new())
    }

    #[test]
    fn a_wallpaper_row_belongs_to_the_app_and_follows_its_choice() {
        let m = json!({ "id": "parallax", "ui": { "change": ["#new-chat-background"] },
            "settings": [ { "kind": "wallpaper" } ] });
        let declared = declared(&m, true);
        let wallpaper = &declared[0];
        assert!(!Control::Wallpaper { on: false }.is_value());
        assert_eq!(
            control(wallpaper, &Json::Null, "parallax", None),
            Control::Wallpaper { on: false }
        );
        assert_eq!(
            control(wallpaper, &Json::Null, "parallax", Some("parallax")),
            Control::Wallpaper { on: true },
            "on for the plugin drawing the background"
        );
        assert_eq!(
            control(wallpaper, &Json::Null, "parallax", Some("another")),
            Control::Wallpaper { on: false },
            "off for any other"
        );
    }

    #[test]
    fn a_permission_row_is_the_apps_words() {
        let m = json!({ "id": "appshots", "settings": [
            { "kind": "permission", "permission": "screen-recording" }
        ] });
        let declared = declared(&m, true);
        assert_eq!(
            control(&declared[0], &Json::Null, "appshots", None),
            Control::Permission {
                permission: "screen-recording".into()
            }
        );
        assert!(
            !Control::Permission {
                permission: "screen-recording".into()
            }
            .is_value()
        );
    }

    #[test]
    fn a_plugins_author_reads_in_words() {
        assert_eq!(author_label(&json!({ "kind": "you" })), "You");
        assert_eq!(author_label(&json!({ "kind": "zeron" })), "Zeron");
        assert_eq!(
            author_label(&json!({ "kind": "verified", "username": "surya" })),
            "surya"
        );
        assert_eq!(
            author_label(&json!({ "kind": "verified", "username": "surya", "mine": true })),
            "surya (your other device)"
        );
        assert_eq!(
            author_label(&json!({ "kind": "unknown", "reason": "no key" })),
            "Unsigned"
        );
        assert_eq!(author_label(&Json::Null), "Unsigned");
    }

    #[test]
    fn a_plugins_state_says_whether_it_can_be_flipped_here() {
        assert_eq!(status_label("on"), "On");
        assert_eq!(status_label("pending"), "Waiting for you");
        assert!(can_toggle("on") && can_toggle("off"));
        assert!(!can_toggle("pending") && !can_toggle("tampered") && !can_toggle("broken"));
    }

    #[test]
    fn every_kind_the_store_keeps_has_a_control() {
        // A kind that gains no control here is one the page would drop
        // silently: every declared kind but the app's own rows draws.
        for kind in zeron_engine::plugins::settings::KINDS {
            let declared = declared(
                &json!({ "id": "x", "ui": { "change": ["#new-chat-background"] }, "settings": [
                    { "id": "s", "kind": kind, "label": kind,
                      "options": [{ "value": "a", "label": "A" }],
                      "permission": "screen-recording" }
                ] }),
                true,
            );
            assert_eq!(declared.len(), 1, "{kind} was left out");
            let ctl = control(&declared[0], &Json::Null, "x", None);
            if *kind == "wallpaper" || *kind == "permission" {
                assert!(!ctl.is_value(), "{kind} is the app's own row");
            } else {
                assert!(ctl.is_value(), "{kind} holds a value of the plugin's");
            }
        }
    }

    /// The page as the window draws it: every plugin the engine listed, in
    /// its own block, with its manifest's settings under its name — and a
    /// switch that keeps what it sets in the plugin store.
    #[gpui::test]
    fn the_page_draws_the_plugins_and_keeps_what_their_controls_set(cx: &mut gpui::TestAppContext) {
        use crate::settings::widgets::take_row_titles;
        use gpui::VisualTestContext;

        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(Default::default(), dir.path(), cx);
        });
        // A plugin as it is installed: its folder, its manifest, the item the
        // engine lists, and the plugin store's folder beside them.
        let plugins = dir.path().join("plugins");
        let folder = plugins.join("commit-graph");
        std::fs::create_dir_all(&folder).unwrap();
        let manifest: Json = json!({
            "id": "commit-graph",
            "name": "Commit graph",
            "version": "0.1.0",
            "settings": [
                { "id": "show", "kind": "switch", "label": "Show the graph",
                  "default": true },
                { "id": "all_branches", "kind": "switch", "label": "All branches" },
            ],
        });
        std::fs::write(
            folder.join(MANIFEST_FILE),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let item = json!({
            "id": "commit-graph",
            "name": "Commit graph",
            "version": "0.1.0",
            "description": "A year of the checkout's commits.",
            "status": "on",
            "author": { "kind": "you" },
            "permissions": ["ui.add", "files.read"],
            "dir": folder.display().to_string(),
        });
        let storage = zeron_engine::plugins::settings::storage_in(&plugins);
        let row = PluginRow::from_item(&item, Some(&manifest), &storage);
        assert_eq!(row.settings.value("show"), Some(json!(true)));

        let data_dir = plugins.parent().unwrap().to_path_buf();
        let (_page, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            PluginsPage::for_test(state, data_dir, vec![row.clone()], cx)
        });
        cx.simulate_resize(gpui::size(px(1200.0), px(800.0)));
        draw(cx);
        let titles = take_row_titles();
        for title in ["Commit graph", "Show the graph", "All branches"] {
            assert!(
                titles.iter().any(|t| t == title),
                "the page drew {titles:?}, without {title:?}"
            );
        }

        // The switch's hit target, pressed: the store keeps what it sets.
        let bounds = cx
            .debug_bounds("plugin-setting-hit-commit-graph:show")
            .expect("the switch's hit target");
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let kept: Json = serde_json::from_str(
            &std::fs::read_to_string(storage.join("commit-graph.settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            kept,
            json!({ "show": false }),
            "off, kept by the plugin store"
        );
        _page.read_with(cx, |page, _| {
            assert_eq!(
                page.rows[0].settings.value("show"),
                Some(json!(false)),
                "the page shows what it set"
            );
            assert_eq!(
                page.rows[0].settings.value("all_branches"),
                Some(json!(false)),
                "the default the manifest declares"
            );
        });
        // And the row reads "Off" back from the store, as a new visit does.
        let reread = PluginRow::from_item(&item, Some(&manifest), &storage);
        assert_eq!(reread.settings.value("show"), Some(json!(false)));

        // Turned off, the plugin keeps its block and loses its settings: it
        // reads none of them while it draws nothing.
        _page.update(cx, |page, cx| {
            page.rows[0].status = "off".to_string();
            cx.notify();
        });
        take_row_titles();
        draw(cx);
        let titles = take_row_titles();
        assert!(titles.iter().any(|t| t == "Commit graph"), "{titles:?}");
        assert!(
            !titles.iter().any(|t| t == "Show the graph"),
            "an off plugin draws no settings: {titles:?}"
        );
    }

    fn draw(cx: &mut gpui::VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
    }

    #[test]
    fn a_manifest_is_read_from_the_file_the_engine_writes() {
        assert_eq!(
            MANIFEST_FILE, "zeron-plugin.json",
            "the engine's manifest file"
        );
        let dir = std::env::temp_dir().join(format!(
            "zeron-plugins-page-manifest-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            read_manifest(&dir).is_none(),
            "no manifest: nothing to read"
        );
        std::fs::write(dir.join(MANIFEST_FILE), "{ not json }").unwrap();
        assert!(read_manifest(&dir).is_none(), "a broken manifest is none");
        std::fs::write(dir.join(MANIFEST_FILE), r#"{"id":"demo","name":"Demo"}"#).unwrap();
        assert_eq!(read_manifest(&dir).unwrap()["name"], "Demo");
        let _ = std::fs::remove_dir_all(dir);
    }
}
