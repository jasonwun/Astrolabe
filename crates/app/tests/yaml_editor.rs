//! Headless UI tests for the YAML editor loop (#21): Enter-indent rules and
//! `- ` list-item continuation, driven through real key dispatch.

mod support;

use astrolabe::session::{ScriptedCluster, Session};
use astrolabe::yaml_editor::{YamlEditorView, register_yaml_language};
use astrolabe_kube::apply::{ApplyMode, ApplyReport};
use astrolabe_kube::cluster::ClusterId;
use futures::channel::mpsc;
use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    AppContext as _, Modifiers, MouseButton, Role, TestAppContext,
    component::input::{Enter, Position},
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
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

type Applied = Rc<RefCell<Vec<ApplyMode>>>;

/// Open the editor over a scripted Cluster that counts its Prechecks and
/// records its Applies.
fn open_counting(
    cx: &mut TestAppContext,
) -> (
    gpui_kit::Entity<YamlEditorView>,
    &mut gpui_kit::VisualTestContext,
    Rc<Cell<usize>>,
    Applied,
) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        astrolabe::keymap::bind(cx);
        register_yaml_language(cx);
    });
    let prechecks = Rc::new(Cell::new(0));
    let applies = Rc::new(RefCell::new(Vec::new()));
    let (_events, receiver) = mpsc::channel(8);
    let scripted = ScriptedCluster::new(receiver)
        .precheck({
            let prechecks = prechecks.clone();
            move |_: &str| {
                prechecks.set(prechecks.get() + 1);
                Ok(ApplyReport::new(vec![]))
            }
        })
        .apply({
            let applies = applies.clone();
            move |_: &str, mode| {
                applies.borrow_mut().push(mode);
                Ok(ApplyReport::new(vec![]))
            }
        });
    let (view, cx) = cx.add_window_view(|window, cx| {
        let session =
            cx.new(|cx| Session::scripted(ClusterId::named("kind-a"), "default", scripted, cx));
        YamlEditorView::with_session(window, cx, "metadata:\n  name: old\n".to_string(), session)
    });
    (view, cx, prechecks, applies)
}

/// #317: Enter on a palette row runs its command.
#[gpui_kit::test]
fn palette_enter_runs_precheck(cx: &mut TestAppContext) {
    let (_view, cx, prechecks, _) = open_counting(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert_eq!(prechecks.get(), 1);
}

#[gpui_kit::test]
fn palette_filtered_enter_runs_precheck(cx: &mut TestAppContext) {
    let (_view, cx, prechecks, _) = open_counting(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_input("precheck");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(prechecks.get(), 1);
}

#[gpui_kit::test]
fn palette_confirm_apply_sends_one_normal_apply(cx: &mut TestAppContext) {
    let (_view, cx, _, applies) = open_counting(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(*applies.borrow(), vec![ApplyMode::Normal]);
}

#[gpui_kit::test]
fn palette_row_click_runs_its_command(cx: &mut TestAppContext) {
    let (_view, cx, prechecks, _) = open_counting(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    let center = cx.update(|window, cx| {
        window.render_frame(cx);
        // Rows: Apply, then Precheck.
        window.find_all_by_role(Role::ListBoxOption)[1]
            .bounds()
            .center()
    });
    cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(prechecks.get(), 1);
}

#[gpui_kit::test]
fn palette_escape_closes_and_returns_focus_to_the_editor(cx: &mut TestAppContext) {
    let (view, cx, prechecks, applies) = open_counting(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let state = cx.read(|cx| view.read(cx).editor_state());
    // Esc ran no command.
    assert_eq!(prechecks.get(), 0);
    assert!(applies.borrow().is_empty());
    cx.read(|cx| {
        assert_eq!(
            state.read(cx).value().as_ref(),
            "\nmetadata:\n  name: old\n"
        )
    });
}
