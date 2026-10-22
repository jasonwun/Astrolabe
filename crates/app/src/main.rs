//! Astrolabe desktop application: GPUI entry point.

use astrolabe::workspace::WorkspaceView;
use astrolabe::yaml_editor::{self, ApplyManifest, RunPrecheck, TogglePalette};
use gpui_kit::*;

fn main() {
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
