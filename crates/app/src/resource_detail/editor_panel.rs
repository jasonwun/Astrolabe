//! The Edit YAML dock tab (#21): the editor loop on one object's manifest.

use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::*;

use crate::yaml_editor::YamlEditorView;

/// The Edit YAML tab: the #21 editor loop on one object's manifest.
pub struct EditorPanel {
    title: String,
    editor: Entity<YamlEditorView>,
}

impl EditorPanel {
    pub fn new(title: String, editor: Entity<YamlEditorView>) -> Self {
        Self { title, editor }
    }
}

impl EventEmitter<PanelEvent> for EditorPanel {}

impl Focusable for EditorPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl BasePanel for EditorPanel {
    fn panel_name(&self) -> &'static str {
        "YamlEditor"
    }
}

impl Panel for EditorPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(format!("Edit {}", self.title).into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        format!("Edit {}", self.title)
    }
}

impl Render for EditorPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("yaml-edit-tab")
            .aria_label(format!("Edit {}", self.title))
            .test_support()
            .size_full()
            .child(self.editor.clone())
    }
}
