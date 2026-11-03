//! Builds objects and runs them through a built-in kind's Registration, as
//! the app does, for the projection tests.
#![allow(dead_code)]

use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::projection::{DetailModel, ResourceTable};

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
