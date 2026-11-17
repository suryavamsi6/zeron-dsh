//! Native visual QA for Settings → Providers: renders the provider cards, the
//! declaration form and the token meter offscreen (never activated, no engine)
//! from a fixture catalog, so the layout can be checked at a narrow window as
//! well as a wide one.
//!
//!   cargo run -p zeron-ui --example provider-cards-fixture --features appshots-fixture -- <output-dir> [name]
//!
//! `ZERON_PALETTE_LIGHT` selects the light palette and `ZERON_FIXTURE_SIZE=<w>x<h>`
//! the window size (default 1100x760; a narrow desktop is the interesting case).
//!
//! Linux has no offscreen readback, so `ZERON_FIXTURE_ONSCREEN=1` puts the
//! window on the display and holds it there instead of capturing: screenshot it
//! with `grim` (Wayland) or `import` (X11).
use gpui::{
    AppContext, AsyncApp, Bounds, Entity, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size, point,
};
use std::{path::PathBuf, time::Duration};
use zeron_engine::dsh_providers::GatewayProvider;
use zeron_ui::*;

async fn pause(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

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

/// The page the cards sit in, without the shell around it.
struct FixturePage {
    cards: Entity<settings::provider_cards::ProviderCards>,
}

impl Render for FixturePage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .p(px(24.0))
            .child(self.cards.clone())
    }
}

fn catalog() -> Vec<GatewayProvider> {
    let entries = serde_json::json!([
        { "provider": "deepseek-official", "displayName": "DeepSeek", "settingsNs": "llm-deepseek", "apiKeyEnv": "DEEPSEEK_API_KEY", "account": true },
        { "provider": "deepseek-account", "displayName": "DeepSeek Account", "settingsNs": "llm-deepseek-account", "account": true },
        { "provider": "chatgpt-subscription", "displayName": "ChatGPT subscription", "settingsNs": "llm-pi-ai", "auth": "chatgpt" },
        { "provider": "opencode-go", "displayName": "OpenCode Go", "settingsNs": "llm-pi-ai", "apiKeyEnv": "OPENCODE_API_KEY" },
        { "provider": "amazon-bedrock", "displayName": "amazon-bedrock", "settingsNs": "llm-pi-ai" },
        { "provider": "anthropic", "displayName": "anthropic", "settingsNs": "llm-pi-ai", "apiKeyEnv": "ANTHROPIC_API_KEY" },
        { "provider": "cloudflare-ai-gateway", "displayName": "cloudflare-ai-gateway", "settingsNs": "llm-pi-ai", "apiKeyEnv": "CLOUDFLARE_API_KEY" },
        { "provider": "acme-gateway", "displayName": "Acme Gateway", "settingsNs": "llm-pi-ai", "apiKeyEnv": "ACME_GATEWAY_API_KEY", "declared": true },
    ]);
    zeron_engine::dsh_providers::parse_provider_catalog(&entries)
}

fn settings_store() -> serde_json::Value {
    serde_json::json!({ "namespaces": [
        { "ns": "agent-default-model", "revision": 0, "value": { "provider": "opencode-go", "model": "deepseek-v4.1-flash" } },
        { "ns": "llm-pi-ai", "revision": 2, "value": { "providers": { "acme-gateway": { "api": "openai-completions" } }, "models": [
            { "id": "deepseek-v4.1-flash", "name": "DeepSeek V4.1 Flash" },
            { "id": "qwen4-max", "name": "Qwen4 Max" },
        ] } },
    ] })
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let mut args = std::env::args().skip(1);
    let output = PathBuf::from(args.next().expect("output directory"));
    let name = args.next().unwrap_or_else(|| "provider-cards".into());
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
            let mut ui_settings = settings::UiSettings::default();
            ui_settings.surface = zeron_theme::SurfacePreference::Opaque;
            ui_settings.save(&data).unwrap();
            settings::init(ui_settings.clone(), data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                ui_settings.ui_font_family.clone(),
                ui_settings.ui_font_size,
                ui_settings.terminal_font_family.clone(),
                ui_settings.terminal_font_size,
                ui_settings.code_font_family.clone(),
                ui_settings.code_font_size,
                fonts,
                cx,
            );
            theme_library::init(data.clone(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                ui_settings.theme_selection,
                ui_settings.accent,
                ui_settings.surface,
                cx,
            );
            composer::init(cx, ui_settings.composer_send_behavior);
            let state = cx.new(|_| state::AppState::new());
            let onscreen = std::env::var_os("ZERON_FIXTURE_ONSCREEN").is_some();
            let origin = if onscreen {
                point(px(60.), px(60.))
            } else {
                point(px(-12000.), px(-12000.))
            };
            let window = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            origin,
                            size(px(width), px(height)),
                        ))),
                        focus: false,
                        show: true,
                        ..Default::default()
                    },
                    |_, cx| {
                        let cards = cx.new(|cx| {
                            settings::provider_cards::ProviderCards::new(state.clone(), cx)
                        });
                        cards.update(cx, |cards, cx| {
                            cards.seed_fixture(catalog(), settings_store(), cx)
                        });
                        cx.new(|_| FixturePage { cards })
                    },
                )
                .unwrap();
            cx.spawn(async move |cx| {
                let any: gpui::AnyWindowHandle = window.into();
                let run: anyhow::Result<()> = async {
                    animate(any, cx, 900).await;
                    if onscreen {
                        // Raise it over whatever else the desktop shows, so an
                        // external screenshot sees this window and nothing else.
                        let _ = any.update(cx, |_, w, _| w.activate_window());
                        pause(cx, 12_000).await;
                    } else {
                        capture(any, cx, &output, &name)?;
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = run {
                    eprintln!("provider cards fixture failed: {error:#}");
                }
                let _ = window.update(cx, |_, w, _| w.remove_window());
                pause(cx, 100).await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    Ok(())
}
