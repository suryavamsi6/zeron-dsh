//! The one place a plugin draws in the GPUI app: a new chat's background
//! (`#new-chat-background`).
//!
//! A plugin declares where it draws in its manifest's `ui.change` and
//! `zeron.place`s a replacement there. This module runs that plugin — its
//! entry and its own modules, through [`zeron_visual::plugin`]'s sandbox —
//! and draws the tree it hands back with native elements: placed boxes,
//! pictures from its folder, and what text it lays.
//!
//! The scene is built again while it moves, with the pointer where it is
//! over the window (`0..1`, both axes) and the time since the last build,
//! so a drawing follows the pointer exactly as its own springs say. It draws
//! only while the plugin is on, the person has chosen it as the wallpaper
//! (`ui-settings.json`'s `pluginWallpaper`), and its replacement is for this
//! place.
//!
//! What the host serves is deliberately a subset of the shipping app's
//! plugin API (see [`zeron_visual::plugin`]); anything else raises an error
//! inside the plugin, which draws nothing rather than a wrong picture.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, img,
    prelude::*, px,
};
use serde_json::Value as Json;
use zeron_visual::plugin::{Modules, PluginScript};
use zeron_visual::{Frame, ImageFit, Node, NodeKind};

use crate::theme::Theme;

/// The place a plugin replaces to draw a new chat's background. A plugin
/// that names it in its manifest's `ui.change` can be the wallpaper.
pub const NEW_CHAT_BACKGROUND: &str = "#new-chat-background";

/// How much of a plugin's `.luau` sources this reads (all together).
const MAX_MODULES_BYTES: usize = 1024 * 1024;

/// Which plugin draws a new chat's background, and where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallpaperPlugin {
    pub id: String,
    pub name: String,
    /// The folder the engine listed (`dir`), where its entry and art are.
    pub dir: PathBuf,
}

impl WallpaperPlugin {
    /// The plugin a `ListPlugins` item names, when it is on and declares
    /// this place in its `ui.change`.
    pub fn from_item(item: &Json) -> Option<Self> {
        let id = item.get("id")?.as_str()?.to_string();
        if item.get("status").and_then(Json::as_str) != Some("on") {
            return None;
        }
        let declared = item.get("ui")?.get("change")?.as_array()?;
        if !declared
            .iter()
            .any(|target| target.as_str() == Some(NEW_CHAT_BACKGROUND))
        {
            return None;
        }
        let dir = item.get("snapshot").or_else(|| item.get("dir"))?.as_str()?;
        Some(Self {
            name: item
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or(&id)
                .to_string(),
            id,
            dir: PathBuf::from(dir),
        })
    }
}

/// What a scene is doing, for the page that offers it as a wallpaper.
#[derive(Debug, Clone, PartialEq)]
pub enum SceneState {
    /// It loaded and replaces the place.
    Drawing,
    /// It couldn't: why not, in one line.
    Failed(SharedString),
}

/// A plugin scene, drawn as a new chat's background.
pub struct PluginScene {
    plugin: WallpaperPlugin,
    script: Option<PluginScript>,
    state: SceneState,
    /// Where the pointer is over the window, `0..1` (none: at rest).
    pointer: Option<(f32, f32)>,
    /// The settings file's stamp when the script last read it.
    stamp: Option<(SystemTime, u64)>,
    /// The tree built for the frame being drawn.
    tree: Option<Rc<Node>>,
    /// Whether the springs were still moving at the last build.
    moving: bool,
    at: Instant,
    /// Pictures decoded from its folder, by path.
    images: HashMap<PathBuf, Option<Arc<gpui::RenderImage>>>,
}

impl PluginScene {
    pub fn new(plugin: WallpaperPlugin, data_dir: &Path, cx: &mut Context<Self>) -> Self {
        let mut scene = Self {
            plugin,
            script: None,
            state: SceneState::Drawing,
            pointer: None,
            stamp: None,
            tree: None,
            moving: false,
            at: Instant::now(),
            images: HashMap::new(),
        };
        scene.reload(data_dir, cx);
        scene
    }

    pub fn plugin(&self) -> &WallpaperPlugin {
        &self.plugin
    }

    pub fn state(&self) -> &SceneState {
        &self.state
    }

    /// Where the pointer is over the window, `0..1` on each axis (none: at
    /// rest, which the plugin reads as the middle).
    pub fn set_pointer(&mut self, pointer: Option<(f32, f32)>, cx: &mut Context<Self>) {
        let pointer = pointer.map(|(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)));
        if self.pointer == pointer {
            return;
        }
        self.pointer = pointer;
        cx.notify();
    }

    /// Load the plugin again: its entry, its modules, its words, and the
    /// settings the person set for it.
    pub fn reload(&mut self, data_dir: &Path, cx: &mut Context<Self>) {
        let storage = zeron_engine::plugins::settings::storage_in(&data_dir.join("plugins"));
        let stamp = settings_stamp(&storage, &self.plugin.id);
        self.stamp = stamp;
        let manifest = crate::settings::plugins::read_manifest(&self.plugin.dir);
        let declared = manifest
            .as_ref()
            .map(|manifest| zeron_engine::plugins::settings::declared(manifest, builtin(&self.plugin)))
            .unwrap_or_default();
        let values = zeron_engine::plugins::settings::values(
            &declared,
            &zeron_engine::plugins::settings::load(&storage, &self.plugin.id),
        );
        let locale = read_locale(&self.plugin.dir);
        let entry_name = manifest
            .as_ref()
            .and_then(|manifest| manifest.get("entry"))
            .and_then(Json::as_str)
            .unwrap_or("init.luau")
            .to_string();
        let reduced = crate::motion::reduced_motion(cx);
        let entry = std::fs::read_to_string(self.plugin.dir.join(&entry_name));
        let script = entry.map_err(|error| format!("Its entry couldn't be read: {error}")).and_then(
            |entry| {
                PluginScript::load(
                    &entry,
                    &read_modules(&self.plugin.dir),
                    &values,
                    &locale,
                    reduced,
                )
                .map_err(|error| format!("Its drawing couldn't load: {}", error.message))
            },
        );
        match script {
            Ok(script) if script.replaces(NEW_CHAT_BACKGROUND) => {
                self.script = Some(script);
                self.state = SceneState::Drawing;
            }
            Ok(_) => {
                self.script = None;
                self.state = SceneState::Failed(SharedString::from(
                    "It doesn't replace a new chat's background.",
                ));
            }
            Err(problem) => {
                self.script = None;
                self.state = SceneState::Failed(SharedString::from(problem));
            }
        }
        self.tree = None;
        self.at = Instant::now();
        cx.notify();
    }

    /// A settings change reloads the plugin: its values are read when it
    /// loads, as the shipping app reads them.
    fn follow_settings(&mut self, data_dir: &Path, cx: &mut Context<Self>) {
        let storage = zeron_engine::plugins::settings::storage_in(&data_dir.join("plugins"));
        if settings_stamp(&storage, &self.plugin.id) != self.stamp {
            self.reload(data_dir, cx);
        }
    }

    /// Build this frame's tree, with the pointer and the time since the
    /// last build.
    fn build(&mut self) {
        let Some(script) = self.script.as_mut() else {
            self.tree = None;
            return;
        };
        let now = Instant::now();
        match script.build(NEW_CHAT_BACKGROUND, self.pointer, now) {
            Ok(tree) => {
                self.moving = script.animating();
                self.tree = tree.map(Rc::new);
                if self.tree.is_none() {
                    self.state = SceneState::Failed(SharedString::from(
                        "It draws nothing at a new chat's background.",
                    ));
                }
            }
            Err(error) => {
                self.state = SceneState::Failed(SharedString::from(error.message));
                self.tree = None;
                self.moving = false;
            }
        }
        self.at = now;
    }
}

impl Render for PluginScene {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data_dir = crate::settings::data_dir(cx).unwrap_or_default();
        self.follow_settings(&data_dir, cx);
        // A still scene with nothing to follow is laid out once.
        if self.moving || self.tree.is_none() {
            self.build();
        }
        if self.moving {
            window.request_animation_frame();
        }
        let theme = Theme::of(cx).clone();
        let mut layer = div().id("new-chat-background").absolute().inset_0();
        if let Some(tree) = self.tree.clone() {
            if let Some(label) = root_label(&tree) {
                layer = layer.role(gpui::Role::Group).aria_label(label);
            }
            layer = layer.child(self.node(&tree, &theme, cx));
        }
        layer
    }
}

impl PluginScene {
    /// One node of the plugin's tree, drawn with the app's own elements.
    fn node(&mut self, node: &Node, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        match &node.kind {
            NodeKind::Frame(frame) => self.frame(frame, theme, cx),
            NodeKind::Image(image) => {
                let fit = match image.fit {
                    ImageFit::Cover => gpui::ObjectFit::Cover,
                    ImageFit::Contain => gpui::ObjectFit::Contain,
                    ImageFit::Fill => gpui::ObjectFit::Fill,
                };
                match self.image(&image.src, cx) {
                    Some(picture) => {
                        let mut element = img(picture).object_fit(fit);
                        if image.width_full {
                            element = element.w_full();
                        }
                        if image.height_full {
                            element = element.h_full();
                        }
                        element.into_any_element()
                    }
                    None => div().into_any_element(),
                }
            }
            NodeKind::Text(text) => {
                let body: String = text.spans.iter().map(|span| span.text.as_str()).collect();
                let color = text
                    .color
                    .map(|color| token_color(theme, color.token))
                    .unwrap_or(theme.text);
                div()
                    .text_size(crate::typography::ui_rems(text.size.metrics().0))
                    .text_color(color)
                    .child(body)
                    .into_any_element()
            }
            NodeKind::Stack(stack) => {
                let mut element = div().flex().gap(px(stack.gap)).w_full();
                element = if stack.axis == zeron_visual::Axis::Horizontal {
                    element.flex_row().items_center()
                } else {
                    element.flex_col()
                };
                for child in &stack.children {
                    element = element.child(self.node(child, theme, cx));
                }
                element.into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    /// A placed box: where it stands (insets, and a spring's offset folded
    /// into them so the picture slides without resizing), what it clips, and
    /// what it holds.
    fn frame(&mut self, frame: &Frame, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let place = &frame.place;
        let mut element = div().flex().flex_col().gap(px(frame.gap));
        element = if place.absolute {
            element.absolute()
        } else {
            element.relative()
        };
        if place.left.is_some() || place.x != 0.0 {
            element = element.left(px(place.left.unwrap_or(0.0) + place.x));
        }
        if let Some(right) = place.right {
            element = element.right(px(right - place.x));
        }
        if place.top.is_some() || place.y != 0.0 {
            element = element.top(px(place.top.unwrap_or(0.0) + place.y));
        }
        if let Some(bottom) = place.bottom {
            element = element.bottom(px(bottom - place.y));
        }
        if frame.width_full {
            element = element.w_full();
        }
        if frame.height_full {
            element = element.h_full();
        }
        if place.clip {
            element = element.overflow_hidden();
        }
        for child in &frame.children {
            element = element.child(self.node(child, theme, cx));
        }
        element.into_any_element()
    }

    /// A picture from the plugin's folder, decoded once.
    fn image(&mut self, src: &str, _cx: &mut Context<Self>) -> Option<Arc<gpui::RenderImage>> {
        let path = plugin_path(&self.plugin.dir, src)?;
        if let Some(cached) = self.images.get(&path) {
            return cached.clone();
        }
        let decoded = std::fs::read(&path)
            .ok()
            .and_then(|bytes| crate::new_thread_background_image::decode(&bytes).ok())
            .map(|picture| {
                Arc::new(gpui::RenderImage::new([image::Frame::new(
                    picture.to_rgba8(),
                )]))
            });
        self.images.insert(path, decoded.clone());
        decoded
    }
}

/// A plugin's folder for the engine's listing: `plugins-builtin` folders
/// are Zeron's own, which may declare its own rows.
fn builtin(plugin: &WallpaperPlugin) -> bool {
    plugin.dir.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("plugins-builtin")
    })
}

/// A settings file's stamp: read again only as it changes.
fn settings_stamp(storage: &Path, id: &str) -> Option<(SystemTime, u64)> {
    let path = zeron_engine::plugins::settings::path_in(storage, id)?;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Every `.luau` file in `dir`, by its name without the extension, so a
/// `require("./scenes")` finds `scenes.luau`.
fn read_modules(dir: &Path) -> Modules {
    let mut modules = Modules::new();
    let mut budget = MAX_MODULES_BYTES;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                // (Pictures and models aren't modules; their folders are
                // large and hold no source.)
                if entry.file_name().to_string_lossy() != "art" {
                    stack.push(path);
                }
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("luau") {
                continue;
            }
            let name = path
                .strip_prefix(dir)
                .ok()
                .map(|rel| rel.with_extension(""))
                .map(|rel| rel.to_string_lossy().replace('\\', "/"));
            let Some(name) = name else { continue };
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            if source.len() > budget {
                break;
            }
            budget -= source.len();
            modules.insert(name, source);
        }
    }
    modules
}

/// Its words, as `scene.violet_dusk` → "Dusk": `locales/en.json`, flattened.
fn read_locale(dir: &Path) -> std::collections::BTreeMap<String, String> {
    let mut words = std::collections::BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(dir.join("locales/en.json")) else {
        return words;
    };
    let Ok(json) = serde_json::from_str::<Json>(&text) else {
        return words;
    };
    fn walk(prefix: &str, value: &Json, out: &mut std::collections::BTreeMap<String, String>) {
        match value {
            Json::Object(map) => {
                for (key, value) in map {
                    let key = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    walk(&key, value, out);
                }
            }
            Json::String(text) => {
                out.insert(prefix.to_string(), text.clone());
            }
            _ => {}
        }
    }
    walk("", &json, &mut words);
    words
}

/// A drawing's theme token, as the app's own color.
fn token_color(theme: &Theme, token: zeron_visual::Token) -> gpui::Hsla {
    use zeron_visual::Token;
    match token {
        Token::Text => theme.text,
        Token::Muted => theme.text_muted,
        Token::Faint => theme.text_faint,
        Token::Accent => theme.accent,
        Token::OnAccent => theme.on_accent,
        Token::Success => theme.success,
        Token::Warning => theme.warning,
        Token::Danger => theme.danger,
        Token::Border => theme.border,
        // (A drawing's charts are its own; the app's first series is the
        // one thing to notice.)
        Token::Chart(_) => theme.accent,
        Token::Rgb(rgb) => gpui::rgb(rgb).into(),
    }
}

/// What the drawing says it is, for assistive technology: the first node
/// that names itself (a scene's own `label`).
fn root_label(node: &Node) -> Option<SharedString> {
    match &node.kind {
        NodeKind::Frame(frame) => frame
            .label
            .clone()
            .map(SharedString::from)
            .or_else(|| frame.children.iter().find_map(root_label)),
        NodeKind::Image(image) => image.label.clone().map(SharedString::from),
        _ => None,
    }
}

/// `src` inside the plugin's folder, when it stays inside it.
pub fn plugin_path(dir: &Path, src: &str) -> Option<PathBuf> {
    if src.is_empty() || src.starts_with('/') || src.contains("..") || src.contains('\\') {
        return None;
    }
    let path = dir.join(src);
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests;
