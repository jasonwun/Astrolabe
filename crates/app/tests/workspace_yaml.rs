//! The Workspace's YAML editor applies to the selected Cluster's Session
//! and says "no cluster" while none is selected (#161), as Freelens's
//! create-resource tab applies to the cluster frame it sits in.

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use astrolabe::session::ScriptedCluster;
use astrolabe_kube::apply::{ApplyMode, ApplyReport};
use astrolabe_kube::cluster::ClusterId;
use astrolabe_kube::watch::NamespaceWatchMessage;
use futures::channel::mpsc;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{Keystroke, TestAppContext, VisualTestContext};
use support::{label, open_workspace};

/// Each Apply a scripted Cluster received: the Cluster, the snapshot and
/// the mode.
type Applied = Rc<RefCell<Vec<(&'static str, String, ApplyMode)>>>;

/// How a scripted Cluster answers a plain Apply.
#[derive(Clone, Copy)]
enum Answer {
    Applied,
    /// A 409 from `kubectl`, which offers Force Apply; a Precheck notes
    /// the unknown field `spec.bogus`.
    Conflict,
    /// A Global error (`forbidden`) on each of the documents.
    Forbidden(&'static [&'static str]),
}

/// Cluster `name`, whose Apply records into `applied` and answers with
/// `answer`, and the sender that keeps its Namespace watch open.
fn cluster(
    name: &'static str,
    applied: &Applied,
    answer: Answer,
) -> (
    (ClusterId, ScriptedCluster),
    mpsc::Sender<NamespaceWatchMessage>,
) {
    let (namespaces, events) = mpsc::channel(8);
    let applied = applied.clone();
    let clean = || ApplyReport::new(Vec::new());
    let scripted = ScriptedCluster::new(events)
        .apply(move |snapshot: &str, mode| {
            applied
                .borrow_mut()
                .push((name, snapshot.to_string(), mode));
            Ok(match (answer, mode) {
                (Answer::Conflict, ApplyMode::Normal) => support::conflict_report(snapshot),
                (Answer::Forbidden(docs), _) => support::global_report(docs),
                _ => clean(),
            })
        })
        .precheck(move |snapshot: &str| {
            Ok(match answer {
                Answer::Conflict => support::unknown_field_report(snapshot),
                Answer::Forbidden(docs) => support::global_report(docs),
                Answer::Applied => clean(),
            })
        });
    ((ClusterId::named(name), scripted), namespaces)
}

fn click(cx: &mut VisualTestContext, id: &'static str) {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    });
    cx.run_until_parked();
}

fn drawn(cx: &mut VisualTestContext, id: &'static str) -> bool {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
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
    let applied = Applied::default();
    let (a, _a_namespaces) = cluster("kind-a", &applied, Answer::Applied);
    let (b, _b_namespaces) = cluster("kind-b", &applied, Answer::Applied);
    let cx = open_workspace(cx, vec![a, b]);

    click(cx, "cluster-kind-b");
    edit(cx, "kind: ConfigMap");
    assert_eq!(label(cx, "yaml-status"), "cluster kind-b");
    apply(cx);

    assert_eq!(
        *applied.borrow(),
        [("kind-b", "kind: ConfigMap".to_string(), ApplyMode::Normal)]
    );
    assert_eq!(label(cx, "yaml-status"), "applied");

    // The editor keeps its buffer and follows the selection.
    click(cx, "cluster-kind-a");
    click(cx, "yaml-editor");
    assert_eq!(label(cx, "yaml-status"), "cluster kind-a");
    apply(cx);

    assert_eq!(
        applied.borrow()[1..],
        [("kind-a", "kind: ConfigMap".to_string(), ApplyMode::Normal)]
    );
}

/// Force Apply offered for one Cluster's conflict would overwrite the
/// other Cluster's fields, so selecting another Cluster drops it.
#[gpui_kit::test]
fn selecting_another_cluster_drops_the_last_apply_result(cx: &mut TestAppContext) {
    let applied = Applied::default();
    let (a, _a_namespaces) = cluster("kind-a", &applied, Answer::Conflict);
    let (b, _b_namespaces) = cluster("kind-b", &applied, Answer::Applied);
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

/// Press `keystroke`, then select Cluster `id` before the answer to the
/// keystroke's request arrives.
fn press_then_select(cx: &mut VisualTestContext, keystroke: &'static str, id: &'static str) {
    let keystroke = Keystroke::parse(keystroke).unwrap();
    cx.update(|window, cx| {
        window.dispatch_keystroke(keystroke, cx);
        window.click(id, cx);
    });
    cx.run_until_parked();
}

/// An Apply the previous Cluster answers after another is selected
/// describes that Cluster, so the editor drops it.
#[gpui_kit::test]
fn an_apply_answered_after_another_cluster_is_selected_is_dropped(cx: &mut TestAppContext) {
    let applied = Applied::default();
    let (a, _a_namespaces) = cluster("kind-a", &applied, Answer::Conflict);
    let (b, _b_namespaces) = cluster("kind-b", &applied, Answer::Applied);
    let cx = open_workspace(cx, vec![a, b]);

    edit(cx, "kind: ConfigMap");
    press_then_select(cx, "cmd-shift-enter", "cluster-kind-b");
    click(cx, "yaml-editor");

    assert_eq!(
        *applied.borrow(),
        [("kind-a", "kind: ConfigMap".to_string(), ApplyMode::Normal)]
    );
    assert!(!drawn(cx, "force-apply"));
    assert_eq!(label(cx, "yaml-status"), "cluster kind-b");
}

#[gpui_kit::test]
fn a_precheck_answered_after_another_cluster_is_selected_is_dropped(cx: &mut TestAppContext) {
    let applied = Applied::default();
    let (a, _a_namespaces) = cluster("kind-a", &applied, Answer::Conflict);
    let (b, _b_namespaces) = cluster("kind-b", &applied, Answer::Applied);
    let cx = open_workspace(cx, vec![a, b]);

    edit(cx, "spec:\n  bogus: 1");
    // Kind-a's Precheck notes the unknown field when it is still selected.
    cx.simulate_keystrokes("f6");
    assert_ne!(label(cx, "yaml-precheck"), "");
    press_then_select(cx, "f6", "cluster-kind-b");
    click(cx, "yaml-editor");

    assert_eq!(label(cx, "yaml-precheck"), "");
    assert!(!drawn(cx, "yaml-problems"));
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

/// An Apply that fails Global on one document shows that message.
#[gpui_kit::test]
fn a_manual_precheck_failing_two_documents_globally_shows_both_messages(cx: &mut TestAppContext) {
    let applied = Applied::default();
    let (a, _namespaces) = cluster(
        "kind-a",
        &applied,
        Answer::Forbidden(&["v1 Namespace/a", "v1 Namespace/b"]),
    );
    let cx = open_workspace(cx, vec![a]);

    edit(cx, "kind: ConfigMap");
    cx.simulate_keystrokes("f6");
    cx.run_until_parked();

    assert_eq!(
        label(cx, "global-notice"),
        "v1 Namespace/a: forbidden: v1 Namespace/a\nv1 Namespace/b: forbidden: v1 Namespace/b"
    );
}

#[gpui_kit::test]
fn an_apply_failing_one_document_globally_shows_its_message(cx: &mut TestAppContext) {
    let applied = Applied::default();
    let (a, _namespaces) = cluster("kind-a", &applied, Answer::Forbidden(&["v1 Namespace/a"]));
    let cx = open_workspace(cx, vec![a]);

    edit(cx, "kind: ConfigMap");
    apply(cx);

    assert_eq!(
        label(cx, "global-notice"),
        "v1 Namespace/a: forbidden: v1 Namespace/a"
    );
}

/// With two documents failing Global, the notice keeps both messages, not
/// only the last.
#[gpui_kit::test]
fn an_apply_failing_two_documents_globally_shows_both_messages(cx: &mut TestAppContext) {
    let applied = Applied::default();
    let (a, _namespaces) = cluster(
        "kind-a",
        &applied,
        Answer::Forbidden(&["v1 Namespace/a", "v1 Namespace/b"]),
    );
    let cx = open_workspace(cx, vec![a]);

    edit(cx, "kind: ConfigMap");
    apply(cx);

    assert_eq!(
        label(cx, "global-notice"),
        "v1 Namespace/a: forbidden: v1 Namespace/a\nv1 Namespace/b: forbidden: v1 Namespace/b"
    );
}
