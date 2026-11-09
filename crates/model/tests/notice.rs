use astrolabe_kube::cluster::ClusterError;
use astrolabe_model::notice::{Intent, Notice, failed_message};
use astrolabe_model::session::{ConnectionHealth, STALE_MESSAGE};
use astrolabe_model::session_state::{DISCOVERY_STALE_MESSAGE, SessionStatus};

#[test]
fn failed_messages_name_the_kind_of_failure_and_the_noun() {
    assert_eq!(
        failed_message("list", Some(&ClusterError::unreachable("refused"))),
        "Cannot reach the cluster: refused"
    );
    assert_eq!(
        failed_message("list", Some(&ClusterError::connect("no kubeconfig"))),
        "Cannot connect to the cluster: no kubeconfig"
    );
    assert_eq!(
        failed_message("list", Some(&ClusterError::new("boom"))),
        "Cannot load the list: boom"
    );
    assert_eq!(
        failed_message("object", Some(&ClusterError::new("boom"))),
        "Cannot load the object: boom"
    );
    assert_eq!(
        failed_message("object", None),
        "The watch ended before the object loaded"
    );
}

fn status(
    health: ConnectionHealth,
    namespaces_stale: bool,
    discovery_failed: bool,
) -> SessionStatus {
    SessionStatus {
        health,
        namespaces_stale,
        discovery_failed,
        crd_access_denied: false,
    }
}

fn banner(text: &str) -> Notice {
    Notice::new("namespace-stale", text, Intent::Warning)
}

#[test]
fn a_healthy_session_has_no_banner() {
    assert_eq!(
        status(ConnectionHealth::Healthy, false, false).banner(),
        None
    );
    assert_eq!(
        status(ConnectionHealth::Connecting, false, false).banner(),
        None
    );
}

#[test]
fn a_lost_connection_says_so() {
    assert_eq!(
        status(ConnectionHealth::Stale, true, false).banner(),
        Some(banner(STALE_MESSAGE))
    );
}

#[test]
fn a_failed_discovery_over_a_healthy_connection_says_so() {
    assert_eq!(
        status(ConnectionHealth::Healthy, false, true).banner(),
        Some(banner(DISCOVERY_STALE_MESSAGE))
    );
}

#[test]
fn a_lost_connection_wins_over_a_failed_discovery() {
    assert_eq!(
        status(ConnectionHealth::Stale, true, true).banner(),
        Some(banner(STALE_MESSAGE))
    );
}

#[test]
fn health_has_a_label_an_intent_and_a_tooltip() {
    let healthy = ConnectionHealth::Healthy.indicator();
    assert_eq!(
        (healthy.label, healthy.intent, healthy.tooltip),
        ("Healthy", Intent::Success, "Connected")
    );
    let connecting = ConnectionHealth::Connecting.indicator();
    assert_eq!(
        (connecting.label, connecting.intent, connecting.tooltip),
        ("Connecting", Intent::Warning, "Connecting to cluster")
    );
    let stale = ConnectionHealth::Stale.indicator();
    assert_eq!(
        (stale.label, stale.intent, stale.tooltip),
        ("Stale", Intent::Danger, STALE_MESSAGE)
    );
}

#[test]
fn the_nav_and_row_open_texts_carry_their_element_id_and_intent() {
    use astrolabe_model::detail::OpenRowError;
    use astrolabe_model::list::{CRD_DENIED, crd_denied_notice, open_row_notice};

    assert_eq!(
        crd_denied_notice(),
        Notice::new("nav-crd-denied", CRD_DENIED, Intent::Muted)
    );
    let error = OpenRowError::Reference(astrolabe_model::detail::ReferenceError::MissingName {
        kind: "Pod".to_string(),
    });
    assert_eq!(
        open_row_notice(&error),
        Notice::new("list-notice", error.to_string(), Intent::Warning)
    );
}

#[test]
fn the_picker_empty_reason_is_a_muted_notice() {
    use astrolabe_model::detail_actions::picker_empty_notice;

    assert_eq!(
        picker_empty_notice("No running container"),
        Notice::new("picker-empty", "No running container", Intent::Muted)
    );
}
