//! Closing session dock tabs (#51): the per-tab close button and `cmd-w`,
//! on a scripted cluster. Synthetic objects (plausible shapes, never
//! captured from a real apiserver).

mod support;

use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_model::builtin::BuiltinKind;
use futures::channel::mpsc;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{Bounds, Pixels, TestAppContext};
use support::{
    ApplyAnswer, ExecAnswer, LogsAnswer, Scenario, Ui, detail_tab_shown, key, mouse_click, object,
    open, open_detail, open_namespace_detail, reopen_detail,
};

/// gpui-component's debug selectors for a dock tab and its close button.
const CLOSE_BUTTON: &str = "dock-tab-close-button";
const DETAIL_TAB: &str = "dock-tab-1";
/// A tab the detail opens: Logs, Exec or Edit YAML.
const CHILD_TAB: &str = "dock-tab-2";

/// Draw a frame and read a debug selector's bounds from it.
fn bounds(ui: &mut Ui<'_>, selector: &'static str) -> Option<Bounds<Pixels>> {
    ui.frame(|_, _| {});
    ui.cx.debug_bounds(selector)
}

#[gpui_kit::test]
fn the_close_button_removes_a_detail_tab(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new(), Scenario::default());
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
    bounds(&mut ui, CLOSE_BUTTON).expect("an inactive detail tab keeps its button");
    mouse_click(ui.cx, CLOSE_BUTTON);
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
    let mut ui = open(cx, Vec::new(), Scenario::default());
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
    let mut ui = open(cx, Vec::new(), Scenario::default());
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
        "status": {"phase": "Running", "containerStatuses": [{"name": "app", "image": "x", "imageID": "", "ready": true, "restartCount": 0, "state": {"running": {}}}]}
    }))
}

/// Open `web-1`'s detail, then the tab `open_child` opens from it, and
/// check the child tab carries the close button and `cmd-w` closes it alone.
fn closes_a_child_tab(cx: &mut TestAppContext, open_child: impl FnOnce(&mut Ui<'_>), label: &str) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Pods, "default"), pod_events)],
        Scenario::default(),
    );
    open_detail(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()], "web-1");
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

#[gpui_kit::test]
fn opening_the_same_detail_twice_shows_one_tab(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new(), Scenario::default());
    open_namespace_detail(&mut ui);
    ui.frame(|window, cx| window.click("nav-namespaces", cx));
    ui.settle();
    ui.frame(|window, cx| window.click("row-old", cx));
    ui.settle();

    assert!(detail_tab_shown(&mut ui), "the open detail is selected");
    assert!(bounds(&mut ui, DETAIL_TAB).is_some());
    assert!(bounds(&mut ui, CHILD_TAB).is_none(), "no second detail tab");
}

#[gpui_kit::test]
fn opening_logs_for_the_same_container_twice_shows_one_tab_with_a_new_stream(
    cx: &mut TestAppContext,
) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Pods, "default"), pod_events)],
        Scenario {
            logs: LogsAnswer::Resets,
            ..Scenario::default()
        },
    );
    open_detail(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()], "web-1");
    ui.frame(|window, cx| window.click("detail-logs", cx));
    ui.settle();
    reopen_detail(&mut ui, BuiltinKind::Pods, "web-1");
    ui.frame(|window, cx| window.click("detail-logs", cx));
    ui.settle();

    assert!(bounds(&mut ui, "dock-tab-3").is_none(), "one Logs tab");
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Logs default/web-1 (app)")
                .is_some(),
            "the Logs tab is selected"
        )
    });
    // The scripted stream failed, so the second open follows it again.
    assert_eq!(ui.logged.borrow().len(), 2);
}

#[gpui_kit::test]
fn opening_exec_again_after_the_shell_exited_starts_a_new_shell_in_the_same_tab(
    cx: &mut TestAppContext,
) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Pods, "default"), pod_events)],
        Scenario {
            // The first shell fails; the second exits cleanly.
            exec: vec![ExecAnswer::Fails, ExecAnswer::Exits],
            ..Scenario::default()
        },
    );
    open_detail(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()], "web-1");
    ui.frame(|window, cx| window.click("detail-exec", cx));
    ui.frame(|window, cx| window.click("pick-shell-sh", cx));
    ui.settle();
    reopen_detail(&mut ui, BuiltinKind::Pods, "web-1");
    ui.frame(|window, cx| window.click("detail-exec", cx));
    ui.frame(|window, cx| window.click("pick-shell-bash", cx));
    ui.settle();

    assert!(bounds(&mut ui, "dock-tab-3").is_none(), "one Exec tab");
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Exec default/web-1 (app)")
                .is_some(),
            "the Exec tab is selected"
        );
        // The second shell is the one the tab now shows.
        assert!(window.try_find_by_label("Exited with code 0").is_some());
    });
    assert_eq!(ui.execed.borrow().len(), 2);
}

#[gpui_kit::test]
fn opening_edit_yaml_twice_shows_one_editor_that_keeps_its_state(cx: &mut TestAppContext) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Pods, "default"), pod_events)],
        Scenario {
            apply: ApplyAnswer::Conflict,
            ..Scenario::default()
        },
    );
    open_detail(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()], "web-1");
    ui.frame(|window, cx| window.click("detail-edit", cx));
    ui.settle();
    // An apply leaves a status that a reloaded editor would not have.
    ui.cx.simulate_keystrokes("cmd-shift-enter");
    ui.settle();
    let status = ui.label("yaml-status");
    assert_eq!(status, "apply failed: 1 problem(s)");

    reopen_detail(&mut ui, BuiltinKind::Pods, "web-1");
    ui.frame(|window, cx| window.click("detail-edit", cx));
    ui.settle();

    assert!(bounds(&mut ui, "dock-tab-3").is_none(), "one editor tab");
    assert_eq!(
        ui.label("yaml-status"),
        status,
        "the editor was not reloaded"
    );
}

#[gpui_kit::test]
fn opening_logs_for_two_containers_shows_two_tabs(cx: &mut TestAppContext) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Pods, "default"), pod_events)],
        Scenario::default(),
    );
    let two = object(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-1", "namespace": "default", "uid": "uid-pod"},
        "spec": {"containers": [{"name": "app", "image": "nginx"}, {"name": "sidecar", "image": "envoy"}]},
        "status": {"phase": "Running", "containerStatuses": [
            {"name": "app", "image": "x", "imageID": "", "ready": true, "restartCount": 0, "state": {"running": {}}},
            {"name": "sidecar", "image": "x", "imageID": "", "ready": true, "restartCount": 0, "state": {"running": {}}}]}
    }));
    open_detail(&mut ui, BuiltinKind::Pods, &mut pods, vec![two], "web-1");
    ui.frame(|window, cx| window.click("detail-logs", cx));
    ui.frame(|window, cx| window.click("pick-container-app", cx));
    ui.settle();
    reopen_detail(&mut ui, BuiltinKind::Pods, "web-1");
    ui.frame(|window, cx| window.click("detail-logs", cx));
    ui.frame(|window, cx| window.click("pick-container-sidecar", cx));
    ui.settle();

    assert!(
        bounds(&mut ui, "dock-tab-3").is_some(),
        "a tab per container"
    );
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Logs default/web-1 (sidecar)")
                .is_some()
        )
    });
}

#[gpui_kit::test]
fn a_closed_logs_tab_reopens_as_a_fresh_tab(cx: &mut TestAppContext) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Pods, "default"), pod_events)],
        Scenario::default(),
    );
    open_detail(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()], "web-1");
    ui.frame(|window, cx| window.click("detail-logs", cx));
    ui.settle();
    ui.frame(|window, cx| window.press("cmd-w", cx));
    ui.settle();
    assert!(bounds(&mut ui, CHILD_TAB).is_none());

    ui.frame(|window, cx| window.click("detail-logs", cx));
    ui.settle();

    assert!(bounds(&mut ui, CHILD_TAB).is_some(), "the Logs tab is back");
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Logs default/web-1 (app)")
                .is_some()
        )
    });
}

#[gpui_kit::test]
fn opening_logs_again_while_the_stream_is_live_selects_the_tab_and_keeps_the_stream(
    cx: &mut TestAppContext,
) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Pods, "default"), pod_events)],
        Scenario {
            logs: LogsAnswer::StaysOpen,
            exec: vec![ExecAnswer::StaysOpen],
            ..Scenario::default()
        },
    );
    open_detail(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()], "web-1");
    ui.frame(|window, cx| window.click("detail-logs", cx));
    ui.settle();
    reopen_detail(&mut ui, BuiltinKind::Pods, "web-1");
    ui.frame(|window, cx| window.click("detail-logs", cx));
    ui.settle();

    assert!(bounds(&mut ui, "dock-tab-3").is_none(), "one Logs tab");
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Logs default/web-1 (app)")
                .is_some(),
            "the Logs tab is selected"
        );
        assert!(window.try_find_by_label("Streaming…").is_some());
    });
    assert_eq!(ui.logged.borrow().len(), 1, "no second stream");

    // The stream is the live one: it ends when the test closes it.
    ui.open_streams.close();
    ui.settle();
    ui.frame(|window, _| assert!(window.try_find_by_label("Stream ended").is_some()));
}

#[gpui_kit::test]
fn opening_exec_again_while_the_shell_is_live_selects_the_tab_and_keeps_the_shell(
    cx: &mut TestAppContext,
) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Pods, "default"), pod_events)],
        Scenario {
            logs: LogsAnswer::StaysOpen,
            exec: vec![ExecAnswer::StaysOpen],
            ..Scenario::default()
        },
    );
    open_detail(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()], "web-1");
    ui.frame(|window, cx| window.click("detail-exec", cx));
    ui.frame(|window, cx| window.click("pick-shell-sh", cx));
    ui.settle();
    reopen_detail(&mut ui, BuiltinKind::Pods, "web-1");
    ui.frame(|window, cx| window.click("detail-exec", cx));
    ui.frame(|window, cx| window.click("pick-shell-bash", cx));
    ui.settle();

    assert!(bounds(&mut ui, "dock-tab-3").is_none(), "one Exec tab");
    ui.frame(|window, _| {
        assert!(
            window
                .try_find_by_label("Exec default/web-1 (app)")
                .is_some(),
            "the Exec tab is selected"
        );
        assert!(window.try_find_by_label("Session running").is_some());
    });
    assert_eq!(ui.execed.borrow().len(), 1, "no second shell");
}

/// Open a pod's detail on a Scenario that keeps streams open, open the
/// child tab, check its stream is followed, close the tab, and check
/// nothing follows the stream any more (#341).
fn closing_a_child_tab_releases_its_stream(
    cx: &mut TestAppContext,
    open_child: impl FnOnce(&mut Ui<'_>),
) {
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::Pods, "default"), pod_events)],
        Scenario {
            logs: LogsAnswer::StaysOpen,
            exec: vec![ExecAnswer::StaysOpen],
            ..Scenario::default()
        },
    );
    open_detail(&mut ui, BuiltinKind::Pods, &mut pods, vec![pod()], "web-1");
    open_child(&mut ui);
    ui.settle();
    assert_eq!(ui.open_streams.listened_to(), 1, "the open tab follows it");

    ui.frame(|window, cx| window.press("cmd-w", cx));
    ui.settle();

    assert_eq!(ui.open_streams.listened_to(), 0, "the closed tab let go");
}

#[gpui_kit::test]
fn closing_a_logs_tab_releases_its_stream(cx: &mut TestAppContext) {
    closing_a_child_tab_releases_its_stream(cx, |ui| {
        ui.frame(|window, cx| window.click("detail-logs", cx));
    });
}

#[gpui_kit::test]
fn closing_an_exec_tab_releases_its_session(cx: &mut TestAppContext) {
    closing_a_child_tab_releases_its_stream(cx, |ui| {
        ui.frame(|window, cx| window.click("detail-exec", cx));
        ui.frame(|window, cx| window.click("pick-shell-sh", cx));
    });
}
