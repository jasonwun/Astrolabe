//! Builds objects and runs them through a built-in kind's Registration, as
//! the app does, for the projection tests; and builds Discovery results.
#![allow(dead_code)]

use astrolabe_kube::cluster::ClusterId;
use astrolabe_kube::discovery::ServedResource;
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::kube::runtime::watcher::Event;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::projection::{DetailModel, ResourceTable};
use astrolabe_model::session::{SnapshotChange, Watched};

pub fn object(value: serde_json::Value) -> DynamicObject {
    serde_json::from_value(value).unwrap()
}

/// Lists `raws` through `kind`'s Registration, as the app does.
pub fn list(kind: BuiltinKind, raws: Vec<serde_json::Value>, now: Timestamp) -> ResourceTable {
    let objects: Vec<DynamicObject> = raws.into_iter().map(object).collect();
    (kind.registration().list)(&objects.iter().collect::<Vec<_>>(), now)
}

/// Projects `raw` through `kind`'s Registration, as the app does.
pub fn detail(kind: BuiltinKind, raw: serde_json::Value, now: Timestamp) -> DetailModel {
    (kind.registration().detail)(&object(raw), now).unwrap()
}

/// `kind` served at `group/version`, its plural the lowercased kind + `s`.
pub fn served(group: &str, version: &str, kind: &str, namespaced: bool) -> ServedResource {
    ServedResource {
        resource: ApiResource {
            group: group.to_string(),
            version: version.to_string(),
            api_version: format!("{group}/{version}"),
            kind: kind.to_string(),
            plural: format!("{}s", kind.to_lowercase()),
        },
        namespaced,
    }
}

pub fn namespace(name: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    }
}

pub fn namespace_watch() -> Watched<Namespace> {
    Watched::new(ClusterId::named("kind-test"), "namespace")
}

pub fn relist(watched: &mut Watched<Namespace>, names: &[&str]) -> SnapshotChange {
    watched.apply(Ok(Event::Init));
    for name in names {
        watched.apply(Ok(Event::InitApply(namespace(name))));
    }
    watched.apply(Ok(Event::InitDone))
}
