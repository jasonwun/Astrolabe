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
    assert!(first.is_some());
    assert_eq!(first, TabRequest::Detail(pod("web-0")).key());
    assert_ne!(first, TabRequest::Detail(pod("web-1")).key());
}

#[test]
fn logs_exec_and_editor_requests_have_no_key() {
    let logs = TabRequest::Logs {
        title: "Logs".to_string(),
        request: LogRequest {
            pod: key(),
            container: "app".to_string(),
        },
    };
    let exec = TabRequest::Exec {
        title: "Exec".to_string(),
        request: ExecRequest {
            pod: key(),
            container: "app".to_string(),
            shell: "sh".to_string(),
        },
    };
    let editor = TabRequest::Editor {
        title: "Edit".to_string(),
        yaml: "a: 1".to_string(),
    };
    assert_eq!(logs.key(), None);
    assert_eq!(exec.key(), None);
    assert_eq!(editor.key(), None);
}
