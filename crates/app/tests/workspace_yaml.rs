//! The Workspace's YAML editor applies to the selected Cluster's Session
//! and says "no cluster" while none is selected (#161), as Freelens's
//! create-resource tab applies to the cluster frame it sits in.

mod support;

use astrolabe::session::{OpenStreams, ScriptedCluster};
use astrolabe_kube::apply::ApplyMode;
use astrolabe_kube::cluster::ClusterId;
use astrolabe_kube::watch::NamespaceWatchMessage;
use futures::channel::mpsc;
use gpui_kit::{TestAppContext, VisualTestContext};
use support::{
    ApplyAnswer, PrecheckAnswer, Recorded, Scenario, click, drawn, label, open_workspace,
};

/// Cluster `name` answering as `scenario` declares, what it was asked, and
/// the sender that keeps its Namespace watch open.
fn cluster(
    name: &'static str,
    scenario: &Scenario,
) -> (
    (ClusterId, ScriptedCluster),
    Recorded,
    mpsc::Sender<NamespaceWatchMessage>,
) {
    let (namespaces, events) = mpsc::channel(8);
    let (scripted, recorded) = scenario.cluster(events, &OpenStreams::default());
    ((ClusterId::named(name), scripted), recorded, namespaces)
}

/// A 409 on a plain Apply, which offers Force Apply, and a Precheck that
/// notes an unknown field.
fn conflicting() -> Scenario {
    Scenario {
        apply: ApplyAnswer::Conflict,
        precheck: PrecheckAnswer::UnknownField,
        ..Scenario::default()
    }
}

/// Show the Workspace's YAML editor and type `text` into it.
fn edit(cx: &mut VisualTestContext, text: &str) {
    click(cx, "yaml-editor");
    cx.simulate_input(text);
    cx.run_until_parked();
}

fn apply(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_workspace_editor_applies_to_the_selected_cluster(cx: &mut TestAppContext) {
    let (a, a_recorded, _a_namespaces) = cluster("kind-a", &Scenario::default());
    let (b, b_recorded, _b_namespaces) = cluster("kind-b", &Scenario::default());
    let cx = open_workspace(cx, vec![a, b]);

    click(cx, "cluster-kind-b");
    edit(cx, "kind: ConfigMap");
    assert_eq!(label(cx, "yaml-status"), "cluster kind-b");
    apply(cx);

    assert_eq!(*b_recorded.applied.borrow(), [ApplyMode::Normal]);
    assert_eq!(*b_recorded.applied_snapshots.borrow(), ["kind: ConfigMap"]);
    assert!(a_recorded.applied.borrow().is_empty());
    assert_eq!(label(cx, "yaml-status"), "applied");

    // The editor keeps its buffer and follows the selection.
    click(cx, "cluster-kind-a");
    click(cx, "yaml-editor");
    assert_eq!(label(cx, "yaml-status"), "cluster kind-a");
    apply(cx);

    assert_eq!(*a_recorded.applied.borrow(), [ApplyMode::Normal]);
    assert_eq!(*a_recorded.applied_snapshots.borrow(), ["kind: ConfigMap"]);
    assert_eq!(b_recorded.applied.borrow().len(), 1, "kind-b applied once");
}

/// Force Apply offered for one Cluster's conflict would overwrite the
/// other Cluster's fields, so selecting another Cluster drops it.
#[gpui_kit::test]
fn selecting_another_cluster_drops_the_last_apply_result(cx: &mut TestAppContext) {
    let (a, _a_recorded, _a_namespaces) = cluster("kind-a", &conflicting());
    let (b, _b_recorded, _b_namespaces) = cluster("kind-b", &Scenario::default());
    let cx = open_workspace(cx, vec![a, b]);

    edit(cx, "kind: ConfigMap");
    apply(cx);
    assert!(drawn(cx, "force-apply"));

    click(cx, "cluster-kind-b");
    click(cx, "yaml-editor");

    assert!(!drawn(cx, "force-apply"));
    assert!(!drawn(cx, "yaml-problems"));
    assert_eq!(label(cx, "yaml-status"), "cluster kind-b");
}

#[gpui_kit::test]
fn with_no_cluster_the_workspace_editor_says_so_and_cannot_apply(cx: &mut TestAppContext) {
    let cx = open_workspace(cx, Vec::new());

    edit(cx, "kind: ConfigMap");
    assert_eq!(label(cx, "yaml-status"), "no cluster");
    apply(cx);

    assert_eq!(label(cx, "global-notice"), "cannot apply: no cluster");
    assert_eq!(label(cx, "yaml-status"), "no cluster");
}

/// The Apply's notices and a manual Precheck's are separate groups, drawn
/// in the one banner with the Apply's first (#308).
#[gpui_kit::test]
fn the_banner_draws_the_apply_notice_before_the_precheck_notice(cx: &mut TestAppContext) {
    let cx = open_workspace(cx, Vec::new());

    edit(cx, "kind: ConfigMap");
    apply(cx);
    cx.simulate_keystrokes("f6");
    cx.run_until_parked();

    assert_eq!(
        label(cx, "global-notice"),
        "cannot apply: no cluster\ncannot precheck: no cluster"
    );
}
