//! Headless UI tests for the YAML editor loop (#21): Enter-indent rules and
//! `- ` list-item continuation, driven through real key dispatch.

use astrolabe::yaml_editor::{YamlEditorView, register_yaml_language};
use gpui_kit::{
    TestAppContext,
    component::input::{Enter, Position},
};

/// Open the editor over `initial`, place the cursor, dispatch Enter, and
/// assert the resulting buffer.
fn enter_at(cx: &mut TestAppContext, initial: &str, line: u32, col: u32, expected: &str) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        register_yaml_language(cx);
    });
    let (view, cx) = cx.add_window_view(|window, cx| {
        YamlEditorView::without_cluster(window, cx, initial.to_string())
    });
    let state = cx.read(|cx| view.read(cx).editor_state());
    cx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.set_cursor_position(Position::new(line, col), window, cx);
        });
        window.dispatch_action(
            Box::new(Enter {
                secondary: false,
                shift: false,
            }),
            cx,
        );
    });
    cx.read(|cx| assert_eq!(state.read(cx).value().as_ref(), expected));
}

/// #6's known gap, patched at the app layer: Enter after a `- ` list item
/// continues the bullet at the same indent.
#[gpui_kit::test]
fn enter_after_list_item_continues_bullet(cx: &mut TestAppContext) {
    enter_at(
        cx,
        "spec:\n  containers:\n    - name: a",
        2,
        13,
        "spec:\n  containers:\n    - name: a\n    - ",
    );
}

/// A non-list line gets the plain auto-indent, no bullet.
#[gpui_kit::test]
fn enter_after_plain_key_only_indents(cx: &mut TestAppContext) {
    enter_at(
        cx,
        "spec:\n  replicas: 2",
        1,
        13,
        "spec:\n  replicas: 2\n  ",
    );
}

/// A mapping line ending in `:` nests the next line one level (the YAML
/// `IndentationRules` from `register_yaml_language`).
#[gpui_kit::test]
fn enter_after_colon_line_nests_one_level(cx: &mut TestAppContext) {
    enter_at(cx, "spec:", 0, 5, "spec:\n  ");
}
