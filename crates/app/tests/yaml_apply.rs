//! The detail tab's Edit YAML editor applies and prechecks through its
//! Session (#160), over the scripted cluster in `support`: a plain Apply
//! conflicts with `kubectl`, a Force Apply goes through, and a Precheck
//! finds an unknown field.

mod support;

use astrolabe_kube::apply::ApplyMode;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use support::{Ui, open};

/// Open the Namespace `old`'s detail, then its Edit YAML tab.
fn open_editor(ui: &mut Ui<'_>) {
    ui.namespaces(&["old"]);
    ui.settle();
    ui.frame(|window, cx| window.click("row-old", cx));
    ui.settle();
    ui.frame(|window, cx| window.click("detail-edit", cx));
    ui.settle();
}

fn label(ui: &mut Ui<'_>, id: &'static str) -> Option<String> {
    ui.frame(|window, _| {
        window
            .try_find(id)
            .and_then(|element| element.label().map(str::to_owned))
    })
}

#[gpui_kit::test]
fn a_conflicting_apply_offers_force_apply_which_goes_through(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    open_editor(&mut ui);

    ui.cx.simulate_keystrokes("cmd-shift-enter");
    ui.settle();

    assert_eq!(*ui.applied.borrow(), [ApplyMode::Normal]);
    assert_eq!(
        label(&mut ui, "yaml-status").as_deref(),
        Some("apply failed: 1 problem(s)")
    );
    assert_eq!(
        label(&mut ui, "yaml-problems").as_deref(),
        Some("1 problem(s) from last apply — conflicts with kubectl")
    );

    ui.frame(|window, cx| window.click("force-apply", cx));
    ui.settle();

    assert_eq!(*ui.applied.borrow(), [ApplyMode::Normal, ApplyMode::Force]);
    assert_eq!(label(&mut ui, "yaml-status").as_deref(), Some("applied"));
    assert!(ui.frame(|window, _| window.try_find("force-apply").is_none()));
    assert!(ui.frame(|window, _| window.try_find("yaml-problems").is_none()));
}

#[gpui_kit::test]
fn a_precheck_notes_the_fields_apply_will_ignore(cx: &mut TestAppContext) {
    let mut ui = open(cx, Vec::new());
    open_editor(&mut ui);

    ui.cx.simulate_keystrokes("f6");
    ui.settle();

    assert_eq!(
        label(&mut ui, "yaml-precheck").as_deref(),
        Some("1 unknown/duplicate field(s) — apply will ignore them")
    );
    assert!(ui.applied.borrow().is_empty());
}
