//! Native visual QA for Settings → DeepSeek: renders the production Shell
//! offscreen (never activated, no engine) on that page and captures it.
//!
//!   ZERON_OPEN_ROUTE=settings/providers cargo run -p zeron-ui \
//!     --example deepseek-settings-fixture --features appshots-fixture -- <output-dir> [name]
//!
//!   ZERON_OPEN_ROUTE=settings/general cargo run -p zeron-ui \
//!     --example settings-search-fixture --features appshots-fixture -- <output-dir> [name]
//!
//! `ZERON_PALETTE_LIGHT` selects the light palette, `ZERON_FIXTURE_FROSTED`
//! the frosted surface, and `ZERON_FIXTURE_SIZE=<w>x<h>` the window size.
use gpui::{
    AppContext, AsyncApp, Bounds, KeyDownEvent, Keystroke, PlatformInput, WindowBounds,
    WindowOptions, point, px, size,
};
use std::{path::PathBuf, time::Duration};
use zeron_ui::*;

async fn pause(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Let `ms` pass a frame at a time. An offscreen window may get no display
/// link, and the reveal moves only as frames are drawn.
async fn animate(window: gpui::AnyWindowHandle, cx: &mut AsyncApp, ms: u64) {
    for _ in 0..ms / 16 {
        let _ = window.update(cx, |_, w, cx| {
            w.refresh();
            w.draw(cx).clear();
        });
        pause(cx, 16).await;
    }
}

fn capture(
    window: gpui::AnyWindowHandle,
    cx: &mut AsyncApp,
    directory: &std::path::Path,
    name: &str,
) -> anyhow::Result<()> {
    let path = directory.join(format!("{name}.png"));
    window.update(cx, |_, w, cx| {
        w.draw(cx).clear();
        w.render_to_image()?.save(&path)?;
        anyhow::Ok(())
    })??;
    Ok(())
}

/// A real key press through the window's dispatch.
fn press(window: gpui::AnyWindowHandle, cx: &mut AsyncApp, keystroke: &str) {
    let keystroke = Keystroke::parse(keystroke).expect("keystroke");
    let _ = window.update(cx, |_, w, cx| {
        w.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke,
                is_held: false,
                prefer_character_input: false,
            }),
            cx,
        );
    });
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let mut args = std::env::args().skip(1);
    let output = PathBuf::from(args.next().expect("output directory"));
    let name = args.next().unwrap_or_else(|| "deepseek-settings".into());
    std::fs::create_dir_all(&output)?;
    let (width, height) = std::env::var("ZERON_FIXTURE_SIZE")
        .ok()
        .and_then(|value| {
            let (w, h) = value.split_once('x')?;
            Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
        })
        .unwrap_or((1100.0, 760.0));
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let temp = tempfile::tempdir()?;
    let data = temp.path().to_path_buf();
    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let mut settings = settings::UiSettings::default();
            settings.sidebar_width = 260.0;
            settings.surface = zeron_theme::SurfacePreference::Opaque;
            settings.save(&data).unwrap();
            settings::init(settings.clone(), data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(settings.ui_font_family.clone(), settings.ui_font_size, settings.terminal_font_family.clone(), settings.terminal_font_size, settings.code_font_family.clone(), settings.code_font_size, fonts, cx);
            theme_library::init(data.clone(), cx);
            appearance::init(appearance::AppearanceMode::Dark, settings.theme_selection, settings.accent, settings.surface, cx);
            history::init(settings.git_history_columns, settings.git_history_column_widths, settings.git_history_column_order, settings.git_history_author_display, cx);
            composer::init(cx, settings.composer_send_behavior);
            terminal::panel::init(cx);
            app_menus::init(cx);
            let state = cx.new(|_| {
                let mut s = state::AppState::new();
                s.connection = zeron_proto::view::ConnectionStatus::Ready;
                s.workspace_scope = Some(zeron_proto::WorkspaceScope::Local);
                s.local_device_id = Some("local".into());
                s.devices = vec![serde_json::from_value(serde_json::json!({"id":"local","name":"This device","platform":std::env::consts::OS,"lastSeenAt":null})).unwrap()];
                s.auto_selected = true;
                s.chats_synced = true;
                s.spaces_synced = true;
                s
            });
            let boot = EngineBootConfig { data_dir: data.clone(), ipc_port: 0, edge_url: String::new(), edge_token: None, org_id: None, workos_client_id: None, default_harness: HarnessId::DeepSeek };
            // Wayland and X11 have no offscreen readback here, so the capture
            // helper can park the window out of sight. ZERON_FIXTURE_ONSCREEN
            // puts it on the display instead, for an external screenshot.
            let onscreen = std::env::var_os("ZERON_FIXTURE_ONSCREEN").is_some();
            let origin = if onscreen { point(px(60.), px(60.)) } else { point(px(-12000.), px(-12000.)) };
            let window = cx.open_window(
                WindowOptions { window_background: theme::Theme::of(cx).window_background_appearance(), window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size(px(width), px(height))))), focus: false, show: true, ..Default::default() },
                |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
            ).unwrap();
            state.update(cx, |_, cx| cx.notify());
            cx.spawn(async move |cx| {
                let any: gpui::AnyWindowHandle = window.into();
                let run: anyhow::Result<()> = async {
                    animate(any, cx, 1600).await;
                    if onscreen {
                        pause(cx, 20_000).await;
                    } else {
                        capture(any, cx, &output, &name)?;
                    }
                    Ok(())
                }.await;
                if let Err(error) = run { eprintln!("deepseek settings fixture failed: {error:#}"); }
                let _ = window.update(cx, |_, w, _| w.remove_window());
                pause(cx, 100).await;
                cx.update(|cx| cx.quit());
            }).detach();
        });
    Ok(())
}
