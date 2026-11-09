//! The launch's selection rules (#344): which Cluster opens and what a
//! launch tab opens. They name `ClusterId::named`, which `crates/app/src`
//! may not (`dev/layer-check.sh`), so they live here.

use astrolabe::launch::{LaunchOpening, LaunchResolution, LaunchTab, LaunchTarget, Preselection};
use astrolabe_kube::cluster::ClusterId;
use astrolabe_kube::object::ObjectKey;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::detail::DetailTarget;
use astrolabe_model::session_state::Viewing;
use astrolabe_model::tab::TabRequest;

fn ids(names: &[&str]) -> Vec<ClusterId> {
    names.iter().map(|name| ClusterId::named(*name)).collect()
}

fn target(name: &str, namespace: Option<&str>) -> LaunchTarget {
    LaunchTarget {
        cluster: ClusterId::named(name),
        namespace: namespace.map(str::to_string),
    }
}

fn preselection(cluster: Option<&str>, namespace: Option<&str>) -> Preselection {
    Preselection {
        cluster: cluster.map(str::to_string),
        namespace: namespace.map(str::to_string),
        open: Vec::new(),
    }
}

#[test]
fn a_listed_cluster_opens_in_the_preselected_namespace() {
    let resolved = preselection(Some("b"), Some("ns")).resolve(&ids(&["a", "b"]));
    assert_eq!(resolved, LaunchResolution::Open(target("b", Some("ns"))));
}

#[test]
fn an_unlisted_cluster_falls_back_to_the_first_and_drops_the_namespace() {
    let resolved = preselection(Some("gone"), Some("ns")).resolve(&ids(&["a", "b"]));
    assert_eq!(
        resolved,
        LaunchResolution::Fallback {
            unlisted: "gone".to_string(),
            target: target("a", None)
        }
    );
}

#[test]
fn no_preselected_cluster_opens_the_first_in_the_preselected_namespace() {
    let resolved = preselection(None, Some("ns")).resolve(&ids(&["a", "b"]));
    assert_eq!(resolved, LaunchResolution::Open(target("a", Some("ns"))));
}

#[test]
fn no_listed_cluster_opens_nothing() {
    assert_eq!(
        preselection(None, None).resolve(&[]),
        LaunchResolution::NoCluster
    );
    assert_eq!(
        preselection(Some("x"), None).resolve(&[]),
        LaunchResolution::NoCluster
    );
}

#[test]
fn a_list_tab_shows_its_kind() {
    assert_eq!(
        LaunchTab::List(BuiltinKind::Secrets).opening("ns"),
        LaunchOpening::Show(Viewing::Builtin(BuiltinKind::Secrets))
    );
}

#[test]
fn a_detail_tab_opens_the_object_in_the_given_namespace() {
    assert_eq!(
        LaunchTab::Detail(BuiltinKind::Secrets, "db".to_string()).opening("ns"),
        LaunchOpening::Open(TabRequest::Detail(DetailTarget::Builtin {
            kind: BuiltinKind::Secrets,
            key: ObjectKey::new(Some("ns".to_string()), "db"),
        }))
    );
}
