//! Which tab requests reuse a tab (#303), with no view.

use astrolabe_kube::object::ObjectKey;
use astrolabe_kube::pod::{ExecRequest, LogRequest};
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::detail::DetailTarget;
use astrolabe_model::tab::TabRequest;

fn pod(name: &str) -> DetailTarget {
    DetailTarget::Builtin {
        kind: BuiltinKind::Pods,
        key: ObjectKey::new(Some("default".to_string()), name),
    }
}

fn key() -> ObjectKey {
    ObjectKey::new(Some("default".to_string()), "web-0")
}

#[test]
fn a_detail_request_has_a_key_equal_for_equal_targets() {
    let first = TabRequest::Detail(pod("web-0")).key();
    assert_eq!(first, TabRequest::Detail(pod("web-0")).key());
    assert_ne!(first, TabRequest::Detail(pod("web-1")).key());
}

fn logs(container: &str) -> TabRequest {
    TabRequest::Logs {
        title: "Logs".to_string(),
        request: LogRequest {
            pod: key(),
            container: container.to_string(),
        },
    }
}

fn exec(container: &str, shell: &str) -> TabRequest {
    TabRequest::Exec {
        title: "Exec".to_string(),
        request: ExecRequest {
            pod: key(),
            container: container.to_string(),
            shell: shell.to_string(),
        },
    }
}

fn editor(name: &str, yaml: &str) -> TabRequest {
    TabRequest::Editor {
        target: pod(name),
        title: "Edit".to_string(),
        yaml: yaml.to_string(),
    }
}

#[test]
fn logs_requests_share_a_key_per_container() {
    assert_eq!(logs("app").key(), logs("app").key());
    assert_ne!(logs("app").key(), logs("sidecar").key());
}

#[test]
fn exec_requests_share_a_key_per_container_whatever_the_shell() {
    assert_eq!(exec("app", "sh").key(), exec("app", "bash").key());
    assert_ne!(exec("app", "sh").key(), exec("sidecar", "sh").key());
}

#[test]
fn editor_requests_share_a_key_per_object_whatever_the_text() {
    assert_eq!(editor("web-0", "a: 1").key(), editor("web-0", "a: 2").key());
    assert_ne!(editor("web-0", "a: 1").key(), editor("web-1", "a: 1").key());
}

#[test]
fn each_kind_of_tab_has_its_own_key_for_one_target() {
    let keys = [
        TabRequest::Detail(pod("web-0")).key(),
        logs("app").key(),
        exec("app", "sh").key(),
        editor("web-0", "a: 1").key(),
    ];
    for (i, a) in keys.iter().enumerate() {
        for b in &keys[i + 1..] {
            assert_ne!(a, b);
        }
    }
}
