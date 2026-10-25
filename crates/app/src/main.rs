//! Astrolabe desktop application: GPUI entry point.

use astrolabe::workspace::WorkspaceView;
use astrolabe::{keymap, yaml_editor};
use gpui_kit::*;

fn main() {
    init_tracing();
    astrolabe::application().run(|cx| {
        gpui_kit::init(cx);
        yaml_editor::register_yaml_language(cx);
        keymap::bind(cx);

        // Not raw cx.open_window: the Root it adds paints the theme (#52).
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
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
