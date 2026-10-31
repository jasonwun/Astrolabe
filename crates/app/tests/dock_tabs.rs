//! Closing session dock tabs (#51): the per-tab close button and `cmd-w`,
//! on a scripted cluster. Synthetic objects (plausible shapes, never
//! captured from a real apiserver).

mod support;

use astrolabe::builtin::BuiltinKind;
use astrolabe_kube::kube::core::DynamicObject;
use futures::channel::mpsc;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{Bounds, Modifiers, MouseButton, Pixels, TestAppContext};
use support::{Ui, key, object, open, snapshot};

/// gpui-component's debug selectors for a dock tab and its close button.
const CLOSE_BUTTON: &str = "dock-tab-close-button";
const DETAIL_TAB: &str = "dock-tab-1";
/// A tab the detail opens: Logs, Exec or Edit YAML.
const CHILD_TAB: &str = "dock-tab-2";

/// Open the Namespaces list holding `old`, then `old`'s detail tab.
fn open_namespace_detail(ui: &mut Ui<'_>) {
    ui.namespaces(&["old"]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-old", cx));
    ui.settle();
}

fn detail_tab_shown(ui: &mut Ui<'_>) -> bool {
    ui.frame(|window, _| {
        window
            .try_find("detail-tab")
            .is_some_and(|tab| tab.label() == Some("Namespace old"))
    })
}

/// Draw a frame and read a debug selector's bounds from it.
fn bounds(ui: &mut Ui<'_>, selector: &'static str) -> Option<Bounds<Pixels>> {
    ui.frame(|_, _| {});
    ui.cx.debug_bounds(selector)
}

#[gpui_kit::test]
fn the_close_button_removes_a_detail_tab(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    open_namespace_detail(&mut ui);
    assert!(detail_tab_shown(&mut ui));

    let button = bounds(&mut ui, CLOSE_BUTTON).expect("the detail tab draws a close button");
    let detail = bounds(&mut ui, DETAIL_TAB).expect("the detail tab is in the tab bar");
    assert!(
        detail.contains(&button.center()),
        "the close button sits on the detail tab"
    );

    // Close the detail while the list is displayed: the click must not
    // select the tab it closes.
    ui.frame(|window, cx| window.click("nav-namespaces", cx));
    ui.settle();
    let button = bounds(&mut ui, CLOSE_BUTTON).expect("an inactive detail tab keeps its button");
    ui.cx
        .simulate_mouse_down(button.center(), MouseButton::Left, Modifiers::none());
    ui.cx
        .simulate_mouse_up(button.center(), MouseButton::Left, Modifiers::none());
    ui.settle();

    assert!(!detail_tab_shown(&mut ui));
    assert!(
        bounds(&mut ui, DETAIL_TAB).is_none(),
        "only the list tab is left"
    );
    assert!(
        bounds(&mut ui, CLOSE_BUTTON).is_none(),
        "the list tab, alone, has no close button"
    );
    ui.frame(|window, _| assert!(window.try_find("row-old").is_some()));
}

#[gpui_kit::test]
fn cmd_w_closes_the_active_detail_tab_but_not_the_list(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    open_namespace_detail(&mut ui);
    assert!(detail_tab_shown(&mut ui));

    ui.frame(|window, cx| window.press("cmd-w", cx));
    ui.settle();
    assert!(!detail_tab_shown(&mut ui), "cmd-w closed the active detail");
    assert!(bounds(&mut ui, DETAIL_TAB).is_none());

    // The list is all that is left; cmd-w leaves it (and the window) alone.
    ui.frame(|window, cx| window.press("cmd-w", cx));
    ui.settle();
    ui.frame(|window, _| assert!(window.try_find("row-old").is_some()));
    assert!(bounds(&mut ui, DETAIL_TAB).is_none());
    assert_eq!(
        ui.cx.update(|_, cx| cx.windows().len()),
        1,
        "the window stays"
    );
}

#[gpui_kit::test]
fn a_closed_detail_reopens_as_a_fresh_selected_tab(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    open_namespace_detail(&mut ui);
    ui.frame(|window, cx| window.press("cmd-w", cx));
    ui.settle();
    assert!(!detail_tab_shown(&mut ui));

    ui.frame(|window, cx| window.click("row-old", cx));
    ui.settle();
    assert!(
        detail_tab_shown(&mut ui),
        "the reopened detail is displayed"
    );
    assert!(bounds(&mut ui, DETAIL_TAB).is_some());
}

/// Synthetic single-container Pod (plausible shape, not captured from a
/// server).
fn pod() -> DynamicObject {
    object(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-1", "namespace": "default", "uid": "uid-pod"},
        "spec": {"containers": [{"name": "app", "image": "nginx"}]},
        "status": {"phase": "Running"}
    }))
}

/// Open `web-1`'s detail, then the tab `open_child` opens from it, and
/// check the child tab carries the close button and `cmd-w` closes it alone.
fn closes_a_child_tab(cx: &mut TestAppContext, open_child: impl FnOnce(&mut Ui<'_>), label: &str) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(cx, vec![(key(BuiltinKind::Pods, "default"), pod_events)]);
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(BuiltinKind::Pods.nav_id(), cx));
    snapshot(&mut pods, vec![pod()]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-default/web-1", cx));
    ui.settle();
    open_child(&mut ui);
    ui.settle();
    ui.frame(|window, _| assert!(window.try_find_by_label(label).is_some()));

    // The last close button drawn is the child tab's.
    let button = bounds(&mut ui, CLOSE_BUTTON).expect("the child tab draws a close button");
    let child = bounds(&mut ui, CHILD_TAB).expect("the child tab is in the tab bar");
    assert!(child.contains(&button.center()));

    ui.frame(|window, cx| window.press("cmd-w", cx));
    ui.settle();
    ui.frame(|window, _| assert!(window.try_find_by_label(label).is_none()));
    assert!(bounds(&mut ui, CHILD_TAB).is_none());
    assert!(
        bounds(&mut ui, DETAIL_TAB).is_some(),
        "only the active tab closed"
    );
}

#[gpui_kit::test]
fn cmd_w_closes_a_logs_tab(cx: &mut TestAppContext) {
    closes_a_child_tab(
        cx,
        |ui| ui.frame(|window, cx| window.click("detail-logs", cx)),
        "Logs default/web-1 (app)",
    );
}

#[gpui_kit::test]
fn cmd_w_closes_an_exec_tab(cx: &mut TestAppContext) {
    closes_a_child_tab(
        cx,
        |ui| {
            ui.frame(|window, cx| window.click("detail-exec", cx));
            ui.frame(|window, cx| window.click("pick-shell-sh", cx));
        },
        "Exec default/web-1 (app)",
    );
}

#[gpui_kit::test]
fn cmd_w_closes_an_editor_tab(cx: &mut TestAppContext) {
    closes_a_child_tab(
        cx,
        |ui| ui.frame(|window, cx| window.click("detail-edit", cx)),
        "Edit Pod default/web-1",
    );
}
