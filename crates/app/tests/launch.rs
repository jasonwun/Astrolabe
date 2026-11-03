//! Launch preselection (#111): the workspace opens a chosen cluster in a
//! chosen namespace at launch; sessions opened later start in `default`.

use astrolabe::session::ScriptedCluster;
use astrolabe::workspace::{LaunchTab, Preselection, WorkspaceView};
use astrolabe_kube::cluster::ClusterId;
use astrolabe_kube::watch::{NamespaceWatchMessage, ObjectWatchMessage};
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::session::WatchKey;
use futures::channel::mpsc;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{TestAppContext, VisualTestContext};
use support::{LogCapture, key_on, mouse_click, object, snapshot};

mod support;

type NamespaceSenders = [mpsc::Sender<NamespaceWatchMessage>; 2];

/// Two scripted clusters, `kind-a` then `kind-b`, both still connecting
/// while the returned senders live; `watches` scripts object watches on
/// `kind-b`.
fn launch(
    cx: &mut TestAppContext,
    preselection: Preselection,
    watches: Vec<(WatchKey, mpsc::Receiver<ObjectWatchMessage>)>,
) -> (&mut VisualTestContext, NamespaceSenders) {
    cx.update(gpui_kit::init);
    let (sender_a, namespaces_a) = mpsc::channel(32);
    let (sender_b, namespaces_b) = mpsc::channel(32);
    let mut kind_b = ScriptedCluster::new(namespaces_b);
    for (key, events) in watches {
        kind_b = kind_b.watch(key, events);
    }
    let (_workspace, cx) = cx.add_window_view(|window, cx| {
        WorkspaceView::from_clusters(
            vec![
                (
                    ClusterId::named("kind-a"),
                    ScriptedCluster::new(namespaces_a),
                ),
                (ClusterId::named("kind-b"), kind_b),
            ],
            preselection,
            window,
            cx,
        )
    });
    cx.run_until_parked();
    (cx, [sender_a, sender_b])
}

fn preselect(cluster: &str, namespace: &str) -> Preselection {
    Preselection {
        cluster: Some(cluster.to_string()),
        namespace: Some(namespace.to_string()),
        ..Preselection::default()
    }
}

#[gpui_kit::test]
fn launch_opens_the_preselected_cluster_in_the_preselected_namespace(cx: &mut TestAppContext) {
    let (cx, _senders) = launch(cx, preselect("kind-b", "team-a"), Vec::new());
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find_by_label("kind-a: Not open").is_some());
        assert!(window.try_find_by_label("kind-b: Connecting").is_some());
        assert!(window.try_find_by_label("Namespace: team-a").is_some());
    });
}

#[gpui_kit::test]
fn first_namespaced_watch_of_the_launch_session_uses_the_preselected_namespace(
    cx: &mut TestAppContext,
) {
    // Only `team-a`'s Pod watch is scripted: a watch on any other
    // namespace receives nothing, so the row shows only if the first Pod
    // watch was `team-a`'s.
    let (mut pods_team_a, events) = mpsc::channel(32);
    let (cx, _senders) = launch(
        cx,
        preselect("kind-b", "team-a"),
        vec![(key_on("kind-b", BuiltinKind::Pods, "team-a"), events)],
    );
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("nav-pods", cx);
    });
    snapshot(
        &mut pods_team_a,
        vec![object(serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "web", "namespace": "team-a"},
        }))],
    );
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("row-team-a/web").is_some());
    });
}

#[gpui_kit::test]
fn launch_falls_back_to_the_first_cluster_when_the_preselected_one_is_unknown(
    cx: &mut TestAppContext,
) {
    let log = LogCapture::default();
    let (cx, _senders) = log.scoped(|| launch(cx, preselect("kind-missing", "team-a"), Vec::new()));
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find_by_label("kind-a: Connecting").is_some());
        assert!(window.try_find_by_label("kind-b: Not open").is_some());
        assert!(window.try_find_by_label("Namespace: default").is_some());
    });
    let log = log.text();
    assert!(log.contains("WARN"), "{log}");
    assert!(log.contains("ASTROLABE_CLUSTER=kind-missing"), "{log}");
}

#[gpui_kit::test]
fn a_cluster_opened_after_launch_starts_in_default(cx: &mut TestAppContext) {
    let (cx, _senders) = launch(cx, preselect("kind-b", "team-a"), Vec::new());
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("cluster-kind-a", cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find_by_label("kind-a: Connecting").is_some());
        assert!(window.try_find_by_label("Namespace: default").is_some());
        window.click("cluster-kind-b", cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find_by_label("Namespace: team-a").is_some());
    });
}

#[gpui_kit::test]
fn launch_opens_the_preselected_list_and_detail_tabs(cx: &mut TestAppContext) {
    let (mut config_maps, config_map_events) = mpsc::channel(32);
    let (mut secrets, secret_events) = mpsc::channel(32);
    let preselection = Preselection {
        open: vec![
            LaunchTab::List(BuiltinKind::ConfigMaps),
            LaunchTab::Detail(BuiltinKind::Secrets, "db".to_string()),
        ],
        ..preselect("kind-b", "team-a")
    };
    let (cx, _senders) = launch(
        cx,
        preselection,
        vec![
            (
                key_on("kind-b", BuiltinKind::ConfigMaps, "team-a"),
                config_map_events,
            ),
            (
                key_on("kind-b", BuiltinKind::Secrets, "team-a"),
                secret_events,
            ),
        ],
    );
    snapshot(
        &mut config_maps,
        vec![object(serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "settings", "namespace": "team-a"},
        }))],
    );
    snapshot(
        &mut secrets,
        vec![object(serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {"name": "db", "namespace": "team-a"},
        }))],
    );
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        let detail = window
            .try_find("detail-tab")
            .expect("the detail tab is shown");
        assert_eq!(detail.label(), Some("Secret team-a/db"));
    });
    assert!(cx.debug_bounds("dock-tab-2").is_none(), "one detail tab");
    mouse_click(cx, "dock-tab-0");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("row-team-a/settings").is_some());
    });
}

#[test]
fn launch_tabs_parse_kinds_as_lists_and_kind_slash_name_as_details() {
    // Unknown kinds log a warning: `an_unknown_launch_tab_kind_…` covers
    // them, so this test leaves the callsite to that test's subscriber.
    assert_eq!(
        LaunchTab::parse_all("Deployment, Secret/db,,Service/web"),
        vec![
            LaunchTab::List(BuiltinKind::Deployments),
            LaunchTab::Detail(BuiltinKind::Secrets, "db".to_string()),
            LaunchTab::Detail(BuiltinKind::Services, "web".to_string()),
        ]
    );
    assert_eq!(LaunchTab::parse_all(""), Vec::new());
}

#[test]
fn an_unknown_launch_tab_kind_is_logged_and_skipped() {
    let log = LogCapture::default();
    let tabs = log.scoped(|| LaunchTab::parse_all("Nope/x,Secret/db"));
    assert_eq!(
        tabs,
        vec![LaunchTab::Detail(BuiltinKind::Secrets, "db".to_string())]
    );
    let log = log.text();
    assert!(log.contains("WARN"), "{log}");
    assert!(log.contains("ASTROLABE_OPEN=Nope/x"), "{log}");
}
