//! The application's key bindings, shared by `main` and the UI tests.

use gpui_kit::component::dock::ClosePanel;
use gpui_kit::*;

use crate::session_view::DOCK_KEY_CONTEXT;
use crate::yaml_editor::{self, ApplyManifest, RunPrecheck, TogglePalette};

pub fn bind(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            "cmd-shift-enter",
            ApplyManifest,
            Some(yaml_editor::KEY_CONTEXT),
        ),
        KeyBinding::new("f6", RunPrecheck, Some(yaml_editor::KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-p", TogglePalette, Some(yaml_editor::KEY_CONTEXT)),
        // The focused tab group handles it; a non-closable tab ignores it.
        KeyBinding::new("cmd-w", ClosePanel, Some(DOCK_KEY_CONTEXT)),
    ]);
}
