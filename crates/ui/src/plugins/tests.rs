//! What a plugin's drawing does with the host: which places it may take,
//! which tree it hands back, and what changes when the pointer or its
//! settings do.

use std::path::PathBuf;

use gpui::{AppContext, Context, Entity, Render, TestAppContext, div, prelude::*};
use serde_json::{Value as Json, json};
use zeron_visual::{ImageFit, Node, NodeKind};

use super::{
    NEW_CHAT_BACKGROUND, PluginScene, SceneState, WallpaperPlugin, read_locale, read_modules,
};
use zeron_visual::plugin::PluginScript;

/// The shipping parallax plugin, as it is in this repository.
fn parallax_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/zgx/plugins-builtin/parallax-scenes")
}

/// An item the engine would list for it.
fn item(status: &str, change: &[&str]) -> Json {
    json!({
        "id": "parallax-scenes",
        "name": "Parallax scenes",
        "version": "1.0.0",
        "status": status,
        "author": { "kind": "zeron" },
        "dir": parallax_dir().display().to_string(),
        "snapshot": parallax_dir().display().to_string(),
        "ui": { "change": change },
    })
}

fn load(dir: &std::path::Path, settings: Json) -> PluginScript {
    load_in(dir, settings, true)
}

/// The plugin's program, with the app's setting for motion reduced or not.
fn load_in(dir: &std::path::Path, settings: Json, reduced_motion: bool) -> PluginScript {
    let entry = std::fs::read_to_string(dir.join("init.luau")).expect("its entry");
    PluginScript::load(
        &entry,
        &read_modules(dir),
        &settings.as_object().cloned().unwrap_or_default(),
        &read_locale(dir),
        reduced_motion,
    )
    .expect("the plugin loads")
}

/// Every picture in `node`'s tree, with how far the box holding it has
/// moved from its place (a spring's answer).
fn pictures(node: &Node, out: &mut Vec<(String, f32)>) {
    match &node.kind {
        NodeKind::Image(image) => out.push((image.src.clone(), 0.0)),
        NodeKind::Frame(frame) => {
            for child in &frame.children {
                let mut inner = Vec::new();
                pictures(child, &mut inner);
                if inner.iter().all(|(_, moved)| *moved == 0.0) {
                    for (src, _) in inner {
                        out.push((src, frame.place.x));
                    }
                } else {
                    out.extend(inner);
                }
            }
        }
        NodeKind::Stack(stack) => {
            for child in &stack.children {
                pictures(child, out);
            }
        }
        _ => {}
    }
}

#[test]
fn a_plugin_is_the_wallpaper_only_while_it_is_on_and_takes_the_place() {
    let chosen = WallpaperPlugin::from_item(&item("on", &[NEW_CHAT_BACKGROUND]))
        .expect("an on plugin that replaces the place");
    assert_eq!(chosen.id, "parallax-scenes");
    assert_eq!(chosen.name, "Parallax scenes");
    assert_eq!(chosen.dir, parallax_dir());
    // Off: not offered.
    assert!(WallpaperPlugin::from_item(&item("off", &[NEW_CHAT_BACKGROUND])).is_none());
    // On, but drawing somewhere else: not this place.
    assert!(WallpaperPlugin::from_item(&item("on", &["#chat-home"])).is_none());
    // On, and its listing doesn't say what it changes: not this place.
    let mut item = item("on", &[]);
    item.as_object_mut().unwrap().remove("ui");
    assert!(WallpaperPlugin::from_item(&item).is_none());
}

#[test]
fn parallax_scenes_draws_its_chosen_scene_behind_a_new_chat() {
    let dir = parallax_dir();
    let mut script = load(&dir, json!({ "scene": "violet-dusk", "parallax": "normal" }));
    assert!(script.replaces(NEW_CHAT_BACKGROUND));
    assert_eq!(script.places().len(), 1);
    assert_eq!(script.places()[0].at, NEW_CHAT_BACKGROUND);
    assert!(script.places()[0].fill, "it fills the place it takes");
    let tree = script
        .build(NEW_CHAT_BACKGROUND, Some((0.5, 0.5)), std::time::Instant::now())
        .expect("it builds")
        .expect("a tree");
    // The replacement, its scene box, and one picture per layer: the sky
    // plus what is nearer, in order.
    let mut found = Vec::new();
    pictures(&tree, &mut found);
    let sources: Vec<&str> = found.iter().map(|(src, _)| src.as_str()).collect();
    assert_eq!(
        sources,
        [
            "art/violet-dusk/sky.webp",
            "art/violet-dusk/far.webp",
            "art/violet-dusk/near.webp",
        ]
    );
    // It reached only for Zeron's helpers and its own modules, by the
    // names it asked for.
    let required = script.required();
    for name in ["./zeron", "./scenes", "@ds/motion"] {
        assert!(required.iter().any(|asked| asked == name), "{required:?}");
    }
}

#[test]
fn its_settings_decide_which_scene_and_how_far_it_moves() {
    let dir = parallax_dir();
    let mut script = load_in(&dir, json!({ "scene": "aurora", "parallax": "deep" }), false);
    let tree = script
        .build(NEW_CHAT_BACKGROUND, Some((0.0, 0.5)), std::time::Instant::now())
        .expect("it builds")
        .expect("a tree");
    let mut found = Vec::new();
    pictures(&tree, &mut found);
    assert!(
        found.iter().all(|(src, _)| src.starts_with("art/aurora/")),
        "{found:?}"
    );
    // Deep moves further than normal at the same pointer: the nearest
    // layer's box sits further from its place.
    let mut normal = load_in(&dir, json!({ "scene": "aurora", "parallax": "normal" }), false);
    let normal_tree = normal
        .build(NEW_CHAT_BACKGROUND, Some((0.0, 0.5)), std::time::Instant::now())
        .expect("it builds")
        .expect("a tree");
    let mut moved_normal = Vec::new();
    pictures(&normal_tree, &mut moved_normal);
    let deep = found
        .iter()
        .map(|(_, moved)| moved.abs())
        .fold(0.0f32, f32::max);
    let gentle = moved_normal
        .iter()
        .map(|(_, moved)| moved.abs())
        .fold(0.0f32, f32::max);
    assert!(
        deep > gentle,
        "deep {deep} should move further than normal {gentle}"
    );
}

#[test]
fn the_layers_follow_the_pointer_and_settle() {
    let dir = parallax_dir();
    let mut script = load_in(
        &dir,
        json!({ "scene": "violet-dusk", "parallax": "normal" }),
        false,
    );
    let now = std::time::Instant::now();
    let rest = script
        .build(NEW_CHAT_BACKGROUND, Some((0.5, 0.5)), now)
        .expect("it builds")
        .expect("a tree");
    let mut at_rest = Vec::new();
    pictures(&rest, &mut at_rest);
    assert!(!script.animating(), "at rest, nothing moves");

    // The pointer to the left: every layer but the sky moves right, and the
    // next frames carry it further as the springs answer.
    let mut last = at_rest.clone();
    for step in 0..6 {
        let tree = script
            .build(
                NEW_CHAT_BACKGROUND,
                Some((0.0, 0.5)),
                now + std::time::Duration::from_millis(16 * (step + 1)),
            )
            .expect("it builds")
            .expect("a tree");
        let mut moved = Vec::new();
        pictures(&tree, &mut moved);
        assert_eq!(moved[0].1, 0.0, "the sky is still");
        assert!(
            moved[2].1 > last[2].1,
            "the nearest layer keeps moving: {last:?} then {moved:?}"
        );
        assert!(script.animating(), "a spring is on its way");
        last = moved;
    }
}

#[test]
fn reduced_motion_holds_the_scene_still() {
    let dir = parallax_dir();
    let mut script = load(&dir, json!({ "scene": "violet-dusk", "parallax": "deep" }));
    let now = std::time::Instant::now();
    for step in 0..4 {
        let tree = script
            .build(
                NEW_CHAT_BACKGROUND,
                Some((if step % 2 == 0 { 0.0 } else { 1.0 }, 0.5)),
                now + std::time::Duration::from_millis(16 * step),
            )
            .expect("it builds")
            .expect("a tree");
        let mut found = Vec::new();
        pictures(&tree, &mut found);
        assert!(
            found.iter().all(|(_, moved)| *moved == 0.0),
            "nothing moves: {found:?}"
        );
    }
    assert!(!script.animating(), "there is nothing to settle");
}

#[test]
fn its_pictures_are_fitted_to_their_layer() {
    let dir = parallax_dir();
    let mut script = load(&dir, json!({ "scene": "canyon", "parallax": "off" }));
    let tree = script
        .build(NEW_CHAT_BACKGROUND, Some((0.5, 0.5)), std::time::Instant::now())
        .expect("it builds")
        .expect("a tree");
    fn fits(node: &Node, out: &mut Vec<ImageFit>) {
        match &node.kind {
            NodeKind::Image(image) => out.push(image.fit),
            NodeKind::Frame(frame) => {
                for child in &frame.children {
                    fits(child, out);
                }
            }
            _ => {}
        }
    }
    let mut written = Vec::new();
    fits(&tree, &mut written);
    // Canyon is four layers deep: each picture covers the box it is laid in.
    assert_eq!(written, [ImageFit::Cover; 4]);
}

#[test]
fn a_plugin_folder_cannot_be_escaped() {
    let dir = parallax_dir();
    assert!(super::plugin_path(&dir, "art/violet-dusk/sky.webp").is_some());
    assert!(super::plugin_path(&dir, "../parallax-scenes/init.luau").is_none());
    assert!(super::plugin_path(&dir, "/etc/passwd").is_none());
    assert!(super::plugin_path(&dir, "art/nowhere.webp").is_none());
}

/// The wallpaper resolver's stand-in for the shell: it keeps the scene
/// while it draws and takes the image wallpaper's place once it fails.
struct Page {
    scene: Entity<PluginScene>,
    fell_back: bool,
}

impl Render for Page {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let drawing = !matches!(self.scene.read(cx).state(), SceneState::Failed(_));
        self.fell_back = !drawing;
        if drawing {
            div().child(self.scene.clone()).into_any_element()
        } else {
            div().id("wallpaper").into_any_element()
        }
    }
}

/// A plugin folder with just an entry: `entry` is its whole `init.luau`.
fn plugin_dir(entry: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a plugin folder");
    std::fs::write(dir.path().join("init.luau"), entry).expect("its entry");
    let data = tempfile::tempdir().expect("a data dir");
    (dir, data)
}

fn page_window(
    cx: &mut TestAppContext,
    plugin: WallpaperPlugin,
    data_dir: &std::path::Path,
) -> gpui::WindowHandle<Page> {
    cx.update(|cx| cx.set_global(crate::theme::Theme::dark()));
    cx.add_window(|_, cx| {
        let scene = cx.new(|cx| PluginScene::new(plugin, data_dir, cx));
        Page {
            scene,
            fell_back: false,
        }
    })
}

#[gpui::test]
fn a_pointer_move_rebuilds_the_scene_even_when_it_is_still(cx: &mut TestAppContext) {
    let data = tempfile::tempdir().expect("a data dir");
    cx.update(|cx| cx.set_global(crate::theme::Theme::dark()));
    let plugin = WallpaperPlugin {
        id: "parallax-scenes".to_string(),
        name: "Parallax scenes".to_string(),
        dir: parallax_dir(),
    };
    let window = cx.add_window(|_, cx| PluginScene::new(plugin, data.path(), cx));
    // When it was last built (a frame that reuses the tree leaves it alone)
    // and where its nearest layer sat: an address would prove neither, since
    // a rebuilt tree can be given the one the old tree had.
    let drawn = |window: &gpui::WindowHandle<PluginScene>, cx: &mut TestAppContext| {
        window
            .update(cx, |scene, _, _| {
                let mut found = Vec::new();
                pictures(scene.tree.as_ref().expect("a tree"), &mut found);
                (scene.at, found)
            })
            .unwrap()
    };
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
        .unwrap();
    window
        .update(cx, |scene, _, _| {
            assert!(matches!(scene.state(), SceneState::Drawing));
            assert!(!scene.moving, "at rest, nothing moves");
        })
        .unwrap();
    let before = drawn(&window, cx);
    // Nothing asked for a frame: drawing again reuses the tree as-is.
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
        .unwrap();
    let still = drawn(&window, cx);
    assert_eq!(still, before, "a still scene is built once");
    // The pointer is what the drawing follows, so its move rebuilds even
    // though no spring was moving.
    window
        .update(cx, |scene, _, cx| scene.set_pointer(Some((0.0, 0.5)), cx))
        .unwrap();
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
        .unwrap();
    let after = drawn(&window, cx);
    assert!(after.0 > before.0, "the pointer's move rebuilt the tree");
    assert_ne!(
        after.1, before.1,
        "the drawing moved with the pointer: {:?} then {:?}",
        before.1, after.1
    );
}

#[gpui::test]
fn a_build_that_fails_after_the_plugin_was_chosen_falls_back(cx: &mut TestAppContext) {
    let (dir, data) = plugin_dir(
        r##"
        zeron.place {
            at = "#new-chat-background",
            fill = true,
            replace = function() error("its scene is broken") end,
        }
        "##,
    );
    let plugin = WallpaperPlugin {
        id: "broken".to_string(),
        name: "Broken".to_string(),
        dir: dir.path().to_path_buf(),
    };
    let window = page_window(cx, plugin, data.path());
    // The plugin was chosen and loaded; its build failing makes the scene
    // report the failure and ask for the frame that swaps the wallpaper in.
    let ran = cx
        .update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
            window.simulate_next_frame(cx)
        })
        .unwrap();
    assert!(ran > 0, "a failure asks the page to reconsider");
    window
        .update(cx, |page, _, cx| {
            assert!(matches!(
                page.scene.read(cx).state(),
                SceneState::Failed(_)
            ));
        })
        .unwrap();
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
        .unwrap();
    window
        .update(cx, |page, _, _| assert!(page.fell_back, "the wallpaper shows"))
        .unwrap();
}

#[gpui::test]
fn a_picture_that_will_not_read_falls_back(cx: &mut TestAppContext) {
    let (dir, data) = plugin_dir(
        r##"
        zeron.place {
            at = "#new-chat-background",
            fill = true,
            replace = function()
                return ui.div { ui.image { src = "art/nowhere.webp" } }
            end,
        }
        "##,
    );
    let plugin = WallpaperPlugin {
        id: "missing-picture".to_string(),
        name: "Missing picture".to_string(),
        dir: dir.path().to_path_buf(),
    };
    let window = page_window(cx, plugin, data.path());
    let ran = cx
        .update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
            window.simulate_next_frame(cx)
        })
        .unwrap();
    assert!(ran > 0, "a picture it can't read asks the page to reconsider");
    window
        .update(cx, |page, _, cx| {
            assert!(matches!(
                page.scene.read(cx).state(),
                SceneState::Failed(_)
            ));
        })
        .unwrap();
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
        .unwrap();
    window
        .update(cx, |page, _, _| assert!(page.fell_back, "the wallpaper shows"))
        .unwrap();
}
