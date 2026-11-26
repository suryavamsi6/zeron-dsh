//! Offscreen pictures of the app's screens and the gallery's, for the
//! gallery-vs-app visual audit: each mounted headless (`TestApp`) and drawn
//! by zgx's own GPU renderer into a texture, read back as a PNG. No window
//! opens, so nothing covers the screen and nothing needs Screen Recording.
//!
//! Run on purpose (it's ignored), one shot list at a time:
//!
//! ```sh
//! AUDIT_OUT=/path/to/dir AUDIT_SHOTS=shell,gallery:sidebar \
//!   cargo test --bin zeron-zgx audit_shots -- --ignored --nocapture
//! ```
//!
//! A shot is `<fixture>` (ui/<fixture>_fixture.luau, or a name ending
//! .luau) or `gallery:<page>`, optionally `@<w>x<h>`, `#<target>` (crop to
//! a node's box) and `!<command>` / `=<state>:<json>` / `^<target>` (a click) /
//! `&<target>@<text>` (a field filled) / `+<ms>` (a wait)
//! steps, joined by `|`:
//! `gallery:composer|#gallery-demo@1240x820`. Steps are apart at `|`
//! outside `"…"`, so a target or typed text may hold one, and a part no
//! prefix claims is an error. A state's JSON may instead come from a file,
//! `=<state>:@<path>`: a real payload the engine wrote (a stored task's)
//! holds structural commas outside its strings, which the shot list's own
//! comma split cannot survive inline. `AUDIT_SCALE` (2) is the
//! pixels a point; `AUDIT_DARK=1` the dark scheme where the screen has one.
//! What's transparent (the window's glass) is laid on `AUDIT_BACKDROP`
//! (`#e9e9ee` light, `#1d1d22` dark), the same under both. `AUDIT_WAIT_MS`
//! lets real time pass before the picture, for what comes over the network.
//!
//! `AUDIT_LIVE_MCP=<dir>` turns the `mcp_live` shot (Settings → MCP
//! servers, which a bare shot host can't answer — the page draws "Can't
//! list MCP servers") into a live one: the fixture mounts over an
//! in-process engine on a fresh temp data dir under `<dir>` (created, kept
//! for the run), with `providers::install_mcp` for its host actions.
//! The live shot drives the add itself — fills Name and Command, clicks
//! the page's Save — then proves the engine took it: the row must list,
//! and `<data_dir>/mcp-servers.json` must name the run's
//! `sur-138-live-proof-<pid>`, or the shot fails even though the page
//! drew it. A spec whose steps click the row's Remove instead proves the
//! removal: the row must go and the file must no longer name the
//! sentinel, so the empty state a person sees after their last server is
//! removed — not a stub — is what the shot renders. Custom shots can
//! still drive the page with `&<target>@<text>`, `^<target>` and
//! `*<target>` (`{sentinel}` expands to that same sentinel in all three). The canvas defaults
//! to 900x2600 (the page's real height), and `AUDIT_OUT` unset falls
//! back to `<dir>`:
//!
//! ```sh
//! AUDIT_LIVE_MCP=/tmp/shots AUDIT_SHOTS=mcp_live \
//!   cargo test --bin zeron-zgx audit_shots -- --ignored --nocapture
//! ```
//!
//! Plugins too: `ZERON_ZGX_DEV_PLUGINS` loads plugin folders into each shot
//! (lent the design system, as the app lends it), and `ZERON_ZGX_DEV_GITHUB`
//! plays GitHub from a JSON file (src/plugins `dev_plugins`): GitHub's
//! viewer and board in ui/github_viewer_fixture.luau, github_board_fixture.luau.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::mock::MockHarness;
use zeron_proto::HarnessId;
use zgx_gpu::GpuRenderer;
use zgx_script::{ScriptHost, testing::TestApp};

struct Shot {
    name: String,
    script: PathBuf,
    size: (f32, f32),
    explicit_size: bool,
    crop: Option<String>,
    steps: Vec<Step>,
}

#[derive(Clone)]
enum Step {
    // Real time passing, frames drawn (`+<ms>`): what comes over the network.
    Wait(u64),
    Command(String),
    Click(String),
    // A key pressed (`>Tab`, `>Escape`): keyboard modality, as a person's
    // key would set it (focus rings show after one).
    Key(String),
    // The pointer resting on a target (`*target`): its hover state.
    Hover(String),
    // A field filled (`&<target>@<text>`): the page's own input, so what
    // the text goes on to do — a Save, a search — is the page's doing.
    Type(String, String),
    State(String, serde_json::Value),
}

fn ui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")
}

/// Splits `text` at `separator` outside `"…"`, so a target or a typed
/// value can hold one.
fn split_outside_quotes(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            _ if c == separator && !quoted => parts.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    parts.push(current);
    parts
}

fn parse(spec: &str, dark: bool) -> Shot {
    let mut parts = split_outside_quotes(spec, '|').into_iter();
    let head = parts.next().unwrap_or_default();
    let (head, size, explicit_size) = match head.split_once('@') {
        Some((h, s)) => {
            let (w, hgt) = s.split_once('x').expect("a size is <w>x<h>");
            (
                h,
                (w.parse().expect("width"), hgt.parse().expect("height")),
                true,
            )
        }
        None => (head.as_str(), (1240., 820.), false),
    };
    let mut steps = Vec::new();
    let mut crop = None;
    let (name, script) = if let Some(page) = head.strip_prefix("gallery:") {
        steps.push(Step::State("dark".into(), serde_json::json!(dark)));
        steps.push(Step::State("page".into(), serde_json::json!(page)));
        (format!("gallery-{page}"), ui_dir().join("gallery.luau"))
    } else if head.ends_with(".luau") {
        (
            head.trim_end_matches(".luau").replace('/', "-"),
            ui_dir().join(head),
        )
    } else {
        (
            head.to_string(),
            ui_dir().join(format!("{head}_fixture.luau")),
        )
    };
    let mut name = name;
    for part in parts {
        if let Some(target) = part.strip_prefix('#') {
            crop = Some(format!("#{target}"));
        } else if let Some(target) = part.strip_prefix('^') {
            steps.push(Step::Click(target.to_string()));
        } else if let Some(key) = part.strip_prefix('>') {
            steps.push(Step::Key(key.to_string()));
        } else if let Some(target) = part.strip_prefix('*') {
            steps.push(Step::Hover(target.to_string()));
        } else if let Some(rest) = part.strip_prefix('&') {
            let (target, text) = rest.split_once('@').expect("&<target>@<text>");
            steps.push(Step::Type(target.to_string(), text.to_string()));
        } else if let Some(command) = part.strip_prefix('!') {
            steps.push(Step::Command(command.to_string()));
        } else if let Some(rest) = part.strip_prefix('=') {
            let (state, json) = rest.split_once(':').expect("=<state>:<json>");
            // `@<path>` reads the payload from a file: a real stored
            // answer's commas are structural, not a string's, so no inline
            // JSON carrying them survives the shot list's comma split.
            let value = match json.strip_prefix('@') {
                Some(path) => {
                    let text = std::fs::read_to_string(path)
                        .unwrap_or_else(|error| panic!("a state file {path}: {error}"));
                    serde_json::from_str(&text).expect("a state file's JSON")
                }
                None => serde_json::from_str(json).expect("a state's JSON"),
            };
            steps.push(Step::State(state.to_string(), value));
        } else if let Some(ms) = part.strip_prefix('+') {
            steps.push(Step::Wait(ms.parse().expect("+<milliseconds>")));
        } else if let Some(label) = part.strip_prefix('~') {
            name = format!("{name}-{label}");
        } else {
            panic!("part `{part}` in spec `{spec}` matches no step prefix");
        }
    }
    Shot {
        name,
        script,
        size,
        explicit_size,
        crop,
        steps,
    }
}

fn hex(s: &str) -> [u8; 3] {
    let s = s.trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).expect("a #rrggbb backdrop");
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// An in-process engine on a fresh temp data dir under `AUDIT_LIVE_MCP`'s
/// parent, for the one fixture the shot must prove reaches it. `client`
/// feeds `providers::install_mcp`'s host actions over the engine's
/// in-memory transport, as `mcp_live_tests` does; the runtime is kept for
/// that transport's tasks and for `close`'s shutdown.
struct Live {
    core: EngineCore,
    client: Rc<zeron_rpc::RpcClient>,
    data_dir: PathBuf,
    _dir: tempfile::TempDir,
    rt: tokio::runtime::Runtime,
}

impl Live {
    fn from_env() -> Result<Option<Live>, String> {
        let Ok(parent) = std::env::var("AUDIT_LIVE_MCP") else {
            return Ok(None);
        };
        let parent = PathBuf::from(parent);
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        // Fresh and empty: no seeded mcp-servers.json, so a row can exist
        // only because the page's own Save asked the engine.
        let dir = tempfile::Builder::new()
            .prefix("sur-138-live-mcp-")
            .tempdir_in(&parent)
            .map_err(|e| e.to_string())?;
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(MockHarness { script: Vec::new() }));
        // `assemble` boots the workspace host, which spawns and needs a
        // Tokio context of its own: enter the runtime this Live keeps.
        let core = {
            let _enter = rt.enter();
            EngineCore::assemble(&data_dir, Arc::new(registry), HarnessId::Mock, None)
                .map_err(|e| format!("engine core assembles: {e}"))?
        };
        core.sessions.set_no_ipc_port();
        let client = Rc::new({
            let _enter = rt.enter();
            zeron_rpc::memory_client(core.rpc_service())
        });
        Ok(Some(Live {
            core,
            client,
            data_dir,
            _dir: dir,
            rt,
        }))
    }

    fn close(self) {
        // Shut the engine down while the temp dir still exists: dropping it
        // first removes the files underneath a running engine.
        let Live { core, rt, _dir, .. } = self;
        rt.block_on(async { core.shutdown().await });
        drop(_dir);
    }
}

/// Draws `shot` and writes `<out>/<name>.png`.
fn take(shot: &Shot, out: &Path, scale: f32, dark: bool) -> Result<PathBuf, String> {
    // The live path is this one fixture's: with `AUDIT_LIVE_MCP` set, the
    // page's host actions talk to an in-process engine, so the row it lists
    // exists because the page's own Save reached it. Without the variable
    // `live` is `None` and the shot is the bare-host one it always was.
    let live_shot = shot
        .script
        .file_name()
        .is_some_and(|n| n == "mcp_live_fixture.luau");
    let live = if live_shot { Live::from_env()? } else { None };
    // The proof a live shot types then finds: a name that fits the page's
    // 32-character rule and can't have come from anywhere else.
    let sentinel = format!("sur-138-live-proof-{}", std::process::id());
    // The settings page's list and form are taller than the default canvas;
    // size it as the page has it in a window unless the spec gave a size.
    let (w, h) = if live_shot && !shot.explicit_size {
        (900., 2600.)
    } else {
        shot.size
    };
    let script = shot.script.clone();
    let client = live.as_ref().map(|live| Rc::clone(&live.client));
    let app = TestApp::try_new(|runtime| {
        let host =
            crate::plugins::lend_design_system(ScriptHost::new(runtime, script.clone()), &script);
        // The live page's modules name their packages as ui/.luaurc does
        // (`@screens`, and `@account` for the sessions the settings search
        // reaches); `@ds` is lent above and `@app` is the root.
        let host = if client.is_some() {
            host.package("screens", ui_dir().join("screens"))
                .package("account", ui_dir().join("account"))
        } else {
            host
        };
        // (What a window host answers and a test host doesn't.)
        host.action("appearance", |_| {});
        host.action("close", |_| {});
        // (The network, as every window has it: remote pictures.)
        zgx_run::install_fetcher(&host);
        crate::plugins::dev_plugins(&host);
        // (Agent visuals, run as a window runs them: ui/visuals_fixture.)
        crate::visuals::register(&host);
        if let Some(client) = &client {
            crate::providers::install_mcp(&host, {
                let client = Rc::clone(client);
                move |method, params| {
                    let client = Rc::clone(&client);
                    Box::pin(async move {
                        client
                            .call(method, params)
                            .await
                            .map_err(|error| error.to_string())
                    })
                }
            });
        }
        host.shared("zgx.window.size", zgx_script::window_size_value(w, h));
        host
    })
    .map_err(|e| format!("{}: {e}", shot.name))?;
    app.ui.scene.borrow_mut().resize(w, h);
    let mut gpu = GpuRenderer::new((w * scale).round() as u32, (h * scale).round() as u32)
        .map_err(|e| format!("GPU: {e:?}"))?;
    gpu.set_scale_factor(scale);
    for font in crate::fonts() {
        font.map_err(|e| e.to_string())?
            .install(&mut gpu.text_system().borrow_mut());
    }
    // The text's language, as a window takes it from the locale shown
    // (`AUDIT_TEXT_LANGUAGE=zh-Hans`): which faces draw Han characters.
    if let Ok(language) = std::env::var("AUDIT_TEXT_LANGUAGE") {
        gpu.text_system().borrow_mut().set_language(Some(&language));
    }
    gpu.install_text(&mut app.ui.scene.borrow_mut());
    app.ui.refresh_text_geometry();
    app.frames(10);
    let mut app = app;
    // Real time for what comes over the network, before the steps (a page
    // to click on must have arrived) and again after them.
    let wait = |app: &mut TestApp| {
        if let Some(ms) = std::env::var("AUDIT_WAIT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(50));
                app.frames(1);
            }
            app.frames(10);
        }
    };
    wait(&mut app);
    // A bare live shot drives the add itself; an explicit step list runs
    // as given.
    let mut steps = shot.steps.clone();
    if live_shot && steps.is_empty() {
        steps.push(Step::Type("role:textbox \"Name\"".into(), sentinel.clone()));
        steps.push(Step::Type("role:textbox \"Command\"".into(), "npx".into()));
        steps.push(Step::Click("role:button \"Save\"".into()));
    }
    for step in &steps {
        match step {
            Step::Wait(ms) => {
                let end = std::time::Instant::now() + std::time::Duration::from_millis(*ms);
                while std::time::Instant::now() < end {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    app.frames(1);
                }
            }
            Step::Click(target) => {
                let target = target.replace("{sentinel}", &sentinel);
                app.target(&target)
                    .map_err(|e| format!("{}: click {target}: {e}", shot.name))?;
                app.click(&target);
            }
            Step::Key(key) => {
                app.keys(key);
            }
            Step::Hover(target) => {
                let target = target.replace("{sentinel}", &sentinel);
                app.target(&target)
                    .map_err(|e| format!("{}: hover {target}: {e}", shot.name))?;
                app.hover(&target);
            }
            Step::Type(target, text) => {
                app.target(target)
                    .map_err(|e| format!("{}: type {target}: {e}", shot.name))?;
                app.fill(target, &text.replace("{sentinel}", &sentinel));
            }
            Step::Command(id) => {
                if !app.host.run_command(id) {
                    return Err(format!("{}: no command `{id}`", shot.name));
                }
            }
            Step::State(name, value) => {
                app.host
                    .set_state(name, crate::plugins::json_value(value))
                    .map_err(|e| format!("{}: {name}: {e}", shot.name))?;
            }
        }
        app.settle();
        app.frames(10);
    }
    // The live shot's provenance, before the picture: the page and the
    // engine must agree on the sentinel, and on which side of it the shot
    // stands. A spec that clicks the sentinel's removal (`{sentinel}` in a
    // click target) renders the empty state — the row gone and the file
    // not naming it, the removal itself the proof the row was the page's
    // own (its button would not have been there to click otherwise). Any
    // other live shot proves the add: the row listed and the file naming
    // it. Either way the file is half the proof — a row the page drew
    // alone, or a file entry the page never showed, is not the page
    // reflecting the engine — and either half can land first, so the poll
    // waits for the two to agree rather than for one fixed order.
    if live_shot {
        let removed = steps
            .iter()
            .any(|step| matches!(step, Step::Click(target) if target.contains("{sentinel}")));
        let remove = format!("role:button \"Remove {sentinel}\"");
        let file = live.as_ref().map(|live| live.data_dir.join("mcp-servers.json"));
        let names = |file: &Path| {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            let parsed: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            parsed["servers"].as_array().is_some_and(|servers| {
                servers
                    .iter()
                    .any(|server| server["name"].as_str() == Some(sentinel.as_str()))
            })
        };
        let mut agreed = false;
        for _ in 0..1000 {
            app.frames(1);
            let listed = app.find(&remove).is_some();
            let named = file.as_ref().is_some_and(|file| names(file));
            if listed == named && listed != removed {
                agreed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if !agreed {
            let error = if removed {
                format!(
                    "{}: \"{sentinel}\" still listed or {} still naming it after the page's removal",
                    shot.name,
                    file.as_ref().map(|f| f.display().to_string()).unwrap_or_default()
                )
            } else {
                format!(
                    "{}: \"{sentinel}\" never listed with {} naming it after the page's Save",
                    shot.name,
                    file.as_ref().map(|f| f.display().to_string()).unwrap_or_default()
                )
            };
            if let Some(live) = live {
                live.close();
            }
            return Err(error);
        }
        if let Some(file) = file {
            if removed {
                println!("audit_shots: {} no longer names \"{sentinel}\"", file.display());
            } else {
                println!("audit_shots: {} names \"{sentinel}\"", file.display());
            }
        }
    }
    // Long enough for entrances and springs to finish.
    app.frames(90);
    wait(&mut app);
    if let Some(error) = app.error() {
        eprintln!("audit_shots: {}: script error: {error}", shot.name);
    }
    app.ui.prepare_frame();
    let report = app.ui.scene.borrow_mut().flush();
    // (Everything, not only what changed: the texture starts empty.)
    let _ = report;
    let all = [zgx::scene::Rect {
        x: 0.,
        y: 0.,
        width: w,
        height: h,
    }];
    gpu.render(&app.ui.scene.borrow(), &all)
        .map_err(|e| format!("render: {e:?}"))?;
    let mut rgba = gpu.readback().map_err(|e| format!("readback: {e:?}"))?;
    let (pw, ph) = ((w * scale).round() as u32, (h * scale).round() as u32);

    // The glass, laid on one backdrop (premultiplied over it).
    let back = hex(&std::env::var("AUDIT_BACKDROP")
        .unwrap_or_else(|_| if dark { "#1d1d22" } else { "#e9e9ee" }.into()));
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        for c in 0..3 {
            px[c] = (px[c] as u32 + back[c] as u32 * (255 - a) / 255).min(255) as u8;
        }
        px[3] = 255;
    }

    // Cropped to a node's box.
    let (mut x0, mut y0, mut cw, mut ch) = (0u32, 0u32, pw, ph);
    if let Some(target) = &shot.crop {
        let node = app
            .target(target)
            .map_err(|e| format!("{}: crop {target}: {e}", shot.name))?;
        let b = app.ui.scene.borrow().bounds(node);
        x0 = ((b.x * scale).max(0.)) as u32;
        y0 = ((b.y * scale).max(0.)) as u32;
        cw = ((b.width * scale) as u32).min(pw - x0);
        ch = ((b.height * scale) as u32).min(ph - y0);
    }
    let mut cropped = Vec::with_capacity((cw * ch * 4) as usize);
    for row in y0..y0 + ch {
        let start = ((row * pw + x0) * 4) as usize;
        cropped.extend_from_slice(&rgba[start..start + (cw * 4) as usize]);
    }
    let path = out.join(format!("{}.png", shot.name));
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), cw, ch);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(&cropped)
        .map_err(|e| e.to_string())?;
    // The picture's taken: shut the engine down before its temp dir drops.
    if let Some(live) = live {
        live.close();
    }
    Ok(path)
}

#[test]
#[ignore = "run on purpose: writes pictures for the visual audit"]
fn audit_shots() {
    // With AUDIT_OUT unset a live run's pictures go beside the engine's
    // temp dir, in the AUDIT_LIVE_MCP parent.
    let out = PathBuf::from(
        std::env::var("AUDIT_OUT")
            .or_else(|_| std::env::var("AUDIT_LIVE_MCP"))
            .expect("AUDIT_OUT: where the pictures go"),
    );
    std::fs::create_dir_all(&out).unwrap();
    let scale: f32 = std::env::var("AUDIT_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2.);
    // Dark unless asked otherwise: the fixtures follow this Mac's (dark).
    let dark = std::env::var_os("AUDIT_LIGHT").is_none();
    let shots = std::env::var("AUDIT_SHOTS").expect("AUDIT_SHOTS: the shots, comma-separated");
    let mut failed = Vec::new();
    // (Shots apart at commas outside quotes: a target may hold one.)
    let specs = split_outside_quotes(&shots, ',');
    for spec in specs.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let shot = parse(spec, dark);
        match take(&shot, &out, scale, dark) {
            Ok(path) => println!("audit_shots: {}", path.display()),
            Err(error) => {
                println!("audit_shots: FAILED {error}");
                failed.push(error);
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    // Steps are apart at `|`, but a `|` inside the text a step types is
    // text: this shell command must be typed whole, not cut at the pipe.
    #[test]
    fn a_pipe_inside_quoted_type_text_is_typed_whole() {
        let spec = "mcp_live|&role:textbox \"Command\"@sh -c \"a|b\"|^role:button \"Save\"";
        let shot = parse(spec, true);
        let typed: Vec<(&str, &str)> = shot
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Type(target, text) => Some((target.as_str(), text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(typed, [("role:textbox \"Command\"", "sh -c \"a|b\"")]);
        assert!(
            shot.steps.iter().any(
                |step| matches!(step, Step::Click(target) if target == "role:button \"Save\"")
            ),
            "the step after the quoted text is still a step"
        );
    }

    // A state's payload read from a file (`=<state>:@<path>`): structural
    // commas and all, where the inline form's first one ends the shot.
    #[test]
    fn a_state_step_at_a_file_reads_its_json() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tasks.json");
        std::fs::write(&file, r#"{"tasks":[{"id":"t1","title":"Standup summary"}]}"#).unwrap();
        let spec = format!("shell|=settings_stub_schedule_tasks:@{}", file.display());
        let shot = parse(&spec, true);
        let state = shot.steps.iter().find_map(|step| match step {
            Step::State(name, value) if name == "settings_stub_schedule_tasks" => Some(value.clone()),
            _ => None,
        });
        assert_eq!(
            state,
            Some(serde_json::json!({ "tasks": [{ "id": "t1", "title": "Standup summary" }] }))
        );
    }

    // A part no prefix claims must stop the run and say which part and
    // which spec, rather than vanishing from the shot.
    #[test]
    fn an_unmatched_part_fails_naming_the_part_and_the_spec() {
        let spec = "mcp_live|%not-a-step";
        let payload = match std::panic::catch_unwind(|| parse(spec, true)) {
            Ok(_) => panic!("an unmatched part must fail, not vanish"),
            Err(payload) => payload,
        };
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(message.contains("%not-a-step"), "names the part: {message}");
        assert!(message.contains(spec), "names the spec: {message}");
    }
}
