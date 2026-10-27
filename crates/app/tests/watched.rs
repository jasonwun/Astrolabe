use astrolabe::session::{ConnectionHealth, SnapshotChange, WatchError, Watched};
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::kube::runtime::watcher::Event;

fn namespace(name: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    }
}

fn names(watched: &Watched<Namespace>) -> Vec<String> {
    watched
        .objects()
        .into_iter()
        .map(|namespace| namespace.metadata.name.clone().unwrap())
        .collect()
}

fn namespace_watch() -> Watched<Namespace> {
    Watched::new("kind-test", "namespace")
}

fn relist(watched: &mut Watched<Namespace>, names: &[&str]) -> SnapshotChange {
    watched.apply(Ok(Event::Init));
    for name in names {
        watched.apply(Ok(Event::InitApply(namespace(name))));
    }
    watched.apply(Ok(Event::InitDone))
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
    assert!(!watched.has_snapshot());
    assert_eq!(
        watched.apply(Ok(Event::InitDone)),
        SnapshotChange::FirstSnapshot
    );
    assert!(watched.has_snapshot());

    assert_eq!(relist(&mut watched, &["default"]), SnapshotChange::Changed);
}

#[test]
fn init_after_a_snapshot_marks_stale_until_init_done() {
    let mut watched = namespace_watch();
    watched.apply(Ok(Event::Init));
    assert!(!watched.show_stale_warning());
    assert_eq!(watched.health(), ConnectionHealth::Connecting);
    watched.apply(Ok(Event::InitDone));
    assert_eq!(watched.health(), ConnectionHealth::Healthy);

    watched.apply(Ok(Event::Init));
    assert!(watched.show_stale_warning());
    assert_eq!(watched.health(), ConnectionHealth::Connecting);

    watched.apply(Ok(Event::InitDone));
    assert!(!watched.show_stale_warning());
    assert_eq!(watched.health(), ConnectionHealth::Healthy);
}

#[test]
fn an_error_marks_stale_and_records_forbidden() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);

    assert_eq!(
        watched.apply(Err(WatchError::forbidden("403"))),
        SnapshotChange::Unchanged
    );
    assert!(watched.show_stale_warning());
    assert!(watched.forbidden());
    assert_eq!(watched.health(), ConnectionHealth::Stale);
    assert_eq!(names(&watched), ["default"]);

    watched.apply(Err(WatchError::new("connection reset")));
    assert!(watched.show_stale_warning());
    assert!(!watched.forbidden());

    watched.apply(Err(WatchError::forbidden("403")));
    relist(&mut watched, &["default"]);
    assert!(!watched.forbidden());
    assert!(!watched.show_stale_warning());
}

#[test]
fn an_error_before_any_snapshot_is_not_stale_data() {
    let mut watched = namespace_watch();
    watched.apply(Err(WatchError::forbidden("403")));
    assert!(!watched.show_stale_warning());
    assert!(watched.forbidden());
    assert_eq!(watched.health(), ConnectionHealth::Stale);
}
