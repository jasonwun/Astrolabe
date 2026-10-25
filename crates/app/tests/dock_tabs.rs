//! Closing session dock tabs (#51): the per-tab close button and `cmd-w`,
//! on a scripted cluster. Synthetic objects (plausible shapes, never
//! captured from a real apiserver).

mod support;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{Bounds, Modifiers, MouseButton, Pixels, TestAppContext};
use support::{Ui, open};

/// gpui-component's debug selectors for a dock tab and its close button.
const CLOSE_BUTTON: &str = "dock-tab-close-button";
const DETAIL_TAB: &str = "dock-tab-1";

/// Open the Namespaces list holding `old`, then `old`'s detail tab.
fn open_detail(ui: &mut Ui<'_>) {
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
    open_detail(&mut ui);
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
    open_detail(&mut ui);
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
    open_detail(&mut ui);
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
