//! Headless UI tests for the YAML editor loop (#21): Enter-indent rules and
//! `- ` list-item continuation, driven through real key dispatch.

mod support;

use astrolabe::session::{ScriptedCluster, Session};
use astrolabe::yaml_editor::{YamlEditorView, register_yaml_language};
use astrolabe_kube::cluster::ClusterId;
use futures::channel::mpsc;
use gpui_kit::{
    AppContext as _, TestAppContext,
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
        YamlEditorView::without_session(window, cx, initial.to_string())
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

/// A failed Apply tints the line its error is on: the conflict over
/// `metadata.name` marks the `name:` line.
#[gpui_kit::test]
fn a_failed_apply_marks_its_line(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        astrolabe::keymap::bind(cx);
        register_yaml_language(cx);
    });
    let (_events, receiver) = mpsc::channel(8);
    let scripted = ScriptedCluster::new(receiver)
        .apply(|snapshot: &str, _| Ok(support::conflict_report(snapshot)));
    let (view, cx) = cx.add_window_view(|window, cx| {
        let session =
            cx.new(|cx| Session::scripted(ClusterId::named("kind-a"), "default", scripted, cx));
        YamlEditorView::with_session(window, cx, "metadata:\n  name: old\n".to_string(), session)
    });

    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();

    // Line 2, `  name: old`, without its newline.
    let marks = cx.read(|cx| view.read(cx).apply_mark_ranges(cx));
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0], 10..21);
}
