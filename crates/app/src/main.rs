//! Astrolabe desktop application: GPUI entry point.

use astrolabe::workspace::WorkspaceView;
use astrolabe::yaml_editor::{self, ApplyManifest, RunPrecheck, TogglePalette};
use gpui_kit::*;

fn main() {
    init_tracing();
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        yaml_editor::register_yaml_language(cx);
        cx.bind_keys([
            KeyBinding::new(
                "cmd-shift-enter",
                ApplyManifest,
                Some(yaml_editor::KEY_CONTEXT),
            ),
            KeyBinding::new("f6", RunPrecheck, Some(yaml_editor::KEY_CONTEXT)),
            KeyBinding::new("cmd-shift-p", TogglePalette, Some(yaml_editor::KEY_CONTEXT)),
        ]);

        cx.open_window(WindowOptions::default(), |window, cx| {
            cx.new(|cx| WorkspaceView::new(window, cx))
        })
        .expect("failed to open window");

        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}

/// Installs a `tracing` subscriber filtered by `RUST_LOG`. Without RUST_LOG
/// the level stays at `warn`, so default runs only surface watch failures and
/// other warnings instead of a debug-event flood (#31).
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
