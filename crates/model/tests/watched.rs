use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::kube::runtime::watcher::Event;
mod support;
use support::{namespace, namespace_watch, relist};

use astrolabe_model::session::{ConnectionHealth, SnapshotChange, WatchStatus, Watched};

fn names(watched: &Watched<Namespace>) -> Vec<String> {
    watched
        .objects()
        .map(|namespace| namespace.metadata.name.clone().unwrap())
        .collect()
}

#[test]
fn relist_keeps_the_old_snapshot_until_the_complete_replacement() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default", "kube-system"]);
    assert_eq!(names(&watched), ["default", "kube-system"]);

    watched.apply(Ok(Event::Init));
    watched.apply(Ok(Event::InitApply(namespace("default"))));
    assert_eq!(names(&watched), ["default", "kube-system"]);

    watched.apply(Ok(Event::InitDone));
    assert_eq!(names(&watched), ["default"]);
}

#[test]
fn apply_and_delete_patch_the_committed_snapshot() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);

    assert_eq!(
        watched.apply(Ok(Event::Apply(namespace("team-a")))),
        SnapshotChange::Changed
    );
    assert_eq!(names(&watched), ["default", "team-a"]);
    assert!(watched.get(None, "team-a").is_some());

    assert_eq!(
        watched.apply(Ok(Event::Delete(namespace("default")))),
        SnapshotChange::Changed
    );
    assert_eq!(names(&watched), ["team-a"]);
    assert!(watched.get(None, "default").is_none());
}

#[test]
fn first_init_done_is_the_first_snapshot_and_later_ones_are_changes() {
    let mut watched = namespace_watch();
    assert_eq!(watched.apply(Ok(Event::Init)), SnapshotChange::Unchanged);
    assert_eq!(
        watched.apply(Ok(Event::InitApply(namespace("default")))),
        SnapshotChange::Unchanged
    );
    assert_eq!(watched.status(), WatchStatus::Connecting);
    assert_eq!(
        watched.apply(Ok(Event::InitDone)),
        SnapshotChange::FirstSnapshot
    );
    assert_eq!(watched.status(), WatchStatus::Live);

    assert_eq!(relist(&mut watched, &["default"]), SnapshotChange::Changed);
}

#[test]
fn the_first_snapshot_takes_a_watch_from_connecting_to_live() {
    let mut watched = namespace_watch();
    assert_eq!(watched.status(), WatchStatus::Connecting);
    assert_eq!(watched.status().health(), ConnectionHealth::Connecting);
    assert!(!watched.status().is_stale());

    watched.apply(Ok(Event::Init));
    assert_eq!(watched.status(), WatchStatus::Connecting);

    watched.apply(Ok(Event::InitDone));
    assert_eq!(watched.status(), WatchStatus::Live);
    assert_eq!(watched.status().health(), ConnectionHealth::Healthy);
    assert!(!watched.status().is_stale());
}

#[test]
fn a_relist_shows_the_old_snapshot_as_stale_until_it_completes() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);

    watched.apply(Ok(Event::Init));
    assert_eq!(watched.status(), WatchStatus::Relisting);
    assert_eq!(watched.status().health(), ConnectionHealth::Connecting);
    assert!(watched.status().is_stale());

    watched.apply(Ok(Event::InitDone));
    assert_eq!(watched.status(), WatchStatus::Live);
}

#[test]
fn a_stream_ending_marks_a_snapshot_stale_and_a_relist_recovers() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);

    watched.mark_stale();
    assert_eq!(watched.status(), WatchStatus::Stale);
    assert_eq!(watched.status().health(), ConnectionHealth::Stale);
    assert!(watched.status().is_stale());

    watched.apply(Ok(Event::Init));
    assert_eq!(watched.status(), WatchStatus::Relisting);
    watched.apply(Ok(Event::InitDone));
    assert_eq!(watched.status(), WatchStatus::Live);
}

#[test]
fn a_stream_ending_before_any_snapshot_is_unreachable_not_stale() {
    let mut watched = namespace_watch();

    watched.mark_stale();

    assert_eq!(watched.status(), WatchStatus::Unreachable);
    assert_eq!(watched.status().health(), ConnectionHealth::Stale);
    assert!(!watched.status().is_stale());

    watched.apply(Ok(Event::Init));
    assert_eq!(watched.status(), WatchStatus::Connecting);
}

#[test]
fn a_403_is_forbidden_and_a_snapshot_lifts_it() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);

    assert_eq!(
        watched.apply(Err(ClusterError::forbidden("403"))),
        SnapshotChange::Unchanged
    );
    assert_eq!(
        watched.status(),
        WatchStatus::Forbidden { has_snapshot: true }
    );
    assert!(watched.status().is_forbidden());
    assert!(watched.status().is_stale());
    assert_eq!(watched.status().health(), ConnectionHealth::Stale);
    assert_eq!(names(&watched), ["default"]);

    // The retry's list start and an ended stream keep the denial.
    watched.apply(Ok(Event::Init));
    assert!(watched.status().is_forbidden());
    watched.mark_stale();
    assert!(watched.status().is_forbidden());

    relist(&mut watched, &["default"]);
    assert_eq!(watched.status(), WatchStatus::Live);
}

#[test]
fn another_failure_replaces_a_recorded_denial() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);
    watched.apply(Err(ClusterError::forbidden("403")));

    watched.apply(Err(ClusterError::new("connection reset")));

    assert_eq!(watched.status(), WatchStatus::Stale);
    assert!(!watched.status().is_forbidden());
}

#[test]
fn a_403_before_any_snapshot_is_not_stale_data() {
    let mut watched = namespace_watch();

    watched.apply(Err(ClusterError::forbidden("403")));

    assert_eq!(
        watched.status(),
        WatchStatus::Forbidden {
            has_snapshot: false
        }
    );
    assert!(!watched.status().is_stale());
    assert_eq!(watched.status().health(), ConnectionHealth::Stale);
}

#[test]
fn a_watch_keeps_its_last_error_until_it_is_live_again() {
    let mut watched = namespace_watch();
    assert_eq!(watched.last_error(), None);

    watched.apply(Err(ClusterError::forbidden("403")));
    assert!(watched.last_error().is_some_and(ClusterError::is_forbidden));

    watched.apply(Err(ClusterError::new("connection reset")));
    assert_eq!(
        watched.last_error(),
        Some(&ClusterError::new("connection reset"))
    );

    watched.mark_stale();
    assert!(watched.last_error().is_some());

    relist(&mut watched, &["default"]);
    assert_eq!(watched.last_error(), None);
}

#[test]
fn a_failure_before_any_snapshot_is_unreachable() {
    let mut watched = namespace_watch();

    watched.apply(Err(ClusterError::new("connection refused")));

    assert_eq!(watched.status(), WatchStatus::Unreachable);
}

#[test]
fn the_revision_goes_up_when_the_snapshot_changes() {
    let mut watched = namespace_watch();
    assert_eq!(watched.revision(), 0);

    relist(&mut watched, &["default"]);
    let first = watched.revision();
    assert!(first > 0);

    watched.apply(Ok(Event::Apply(namespace("team-a"))));
    let applied = watched.revision();
    assert!(applied > first);

    watched.apply(Ok(Event::Delete(namespace("team-a"))));
    assert!(watched.revision() > applied);
}

#[test]
fn events_that_leave_the_snapshot_alone_keep_the_revision() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);
    let before = watched.revision();

    // A relist in progress, a failure and a stream end change no object.
    watched.apply(Ok(Event::Init));
    watched.apply(Ok(Event::InitApply(namespace("staged"))));
    watched.apply(Err(ClusterError::new("connection reset")));
    watched.mark_stale();

    assert_eq!(watched.revision(), before);
    assert_eq!(names(&watched), ["default"]);
}

#[test]
fn a_denial_keeps_reading_stale_through_the_retrys_list_start() {
    let mut watched = namespace_watch();
    watched.apply(Err(ClusterError::forbidden("403")));

    watched.apply(Ok(Event::Init));

    assert_eq!(
        watched.status(),
        WatchStatus::Forbidden {
            has_snapshot: false
        }
    );
    assert_eq!(watched.status().health(), ConnectionHealth::Stale);
}

#[test]
fn a_failure_during_a_relist_leaves_the_old_snapshot_stale() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);
    watched.apply(Ok(Event::Init));

    watched.mark_stale();

    assert_eq!(watched.status(), WatchStatus::Stale);
}

fn versioned(name: &str, version: &str) -> Namespace {
    let mut namespace = namespace(name);
    namespace.metadata.resource_version = Some(version.to_string());
    namespace
}

#[test]
fn replaying_the_known_resource_version_changes_nothing() {
    let mut watched = namespace_watch();
    watched.apply(Ok(Event::Apply(versioned("default", "7"))));
    let before = watched.revision();

    let change = watched.apply(Ok(Event::Apply(versioned("default", "7"))));

    assert_eq!(change, SnapshotChange::Unchanged);
    assert_eq!(watched.revision(), before);

    let change = watched.apply(Ok(Event::Apply(versioned("default", "8"))));

    assert_eq!(change, SnapshotChange::Changed);
    assert!(watched.revision() > before);
}

#[test]
fn an_object_without_a_version_is_always_a_change() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);
    let before = watched.revision();

    let change = watched.apply(Ok(Event::Apply(namespace("default"))));

    assert_eq!(change, SnapshotChange::Changed);
    assert!(watched.revision() > before);
}

#[test]
fn deleting_an_absent_object_changes_nothing() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);
    let before = watched.revision();

    let change = watched.apply(Ok(Event::Delete(namespace("ghost"))));

    assert_eq!(change, SnapshotChange::Unchanged);
    assert_eq!(watched.revision(), before);
}

#[test]
fn a_relist_of_identical_objects_is_still_a_change() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);
    let before = watched.revision();

    relist(&mut watched, &["default"]);

    assert!(watched.revision() > before);
}

#[test]
fn objects_come_out_in_key_order_and_get_matches_namespace_and_name() {
    use astrolabe_kube::k8s_openapi::api::core::v1::ConfigMap;

    fn config_map(namespace: Option<&str>, name: &str) -> ConfigMap {
        ConfigMap {
            metadata: ObjectMeta {
                namespace: namespace.map(str::to_string),
                name: Some(name.to_string()),
                ..ObjectMeta::default()
            },
            ..ConfigMap::default()
        }
    }

    let mut watched = Watched::new(ClusterId::named("kind-test"), "configmap");
    watched.apply(Ok(Event::Init));
    for (namespace, name) in [
        (Some("b"), "x"),
        (Some("a"), "z"),
        (None, "m"),
        (Some("a"), "y"),
        (Some("a-2"), "m"),
        (Some("a"), "yy"),
        (Some("é"), "m"),
    ] {
        watched.apply(Ok(Event::InitApply(config_map(namespace, name))));
    }
    watched.apply(Ok(Event::InitDone));

    let order: Vec<_> = watched
        .objects()
        .map(|o| {
            (
                o.metadata.namespace.clone(),
                o.metadata.name.clone().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            (None, "m".to_string()),
            (Some("a".to_string()), "y".to_string()),
            (Some("a".to_string()), "yy".to_string()),
            (Some("a".to_string()), "z".to_string()),
            (Some("a-2".to_string()), "m".to_string()),
            (Some("b".to_string()), "x".to_string()),
            (Some("é".to_string()), "m".to_string()),
        ]
    );
    assert!(watched.get(Some("a-2"), "m").is_some());
    assert!(watched.get(Some("é"), "m").is_some());
    assert!(watched.get(Some("a"), "yy").is_some());
    assert!(watched.get(Some("a"), "z").is_some());
    assert!(watched.get(None, "m").is_some());
    assert!(watched.get(Some("a"), "m").is_none());
    assert!(watched.get(None, "z").is_none());
}
