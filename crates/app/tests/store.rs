use astrolabe::store::Store;
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

fn names(store: &Store<Namespace>) -> Vec<String> {
    store
        .snapshot()
        .into_iter()
        .map(|namespace| namespace.metadata.name.clone().unwrap())
        .collect()
}

#[test]
fn relist_keeps_the_old_snapshot_until_the_complete_replacement() {
    let mut store = Store::new();
    store.apply(Event::Init);
    store.apply(Event::InitApply(namespace("default")));
    store.apply(Event::InitApply(namespace("kube-system")));
    store.apply(Event::InitDone);
    assert_eq!(names(&store), ["default", "kube-system"]);

    store.apply(Event::Init);
    store.apply(Event::InitApply(namespace("default")));
    assert_eq!(names(&store), ["default", "kube-system"]);

    store.apply(Event::InitDone);
    assert_eq!(names(&store), ["default"]);
}
