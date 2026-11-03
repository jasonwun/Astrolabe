//! Watches through the `Cluster` handle over a mock apiserver, on a paused
//! tokio clock: the 1 s and 30 s retries pass at once.

use std::time::Duration;

use astrolabe_kube::cluster::{Cluster, ClusterError, ClusterErrorKind};
use astrolabe_kube::kube::core::Status;
use astrolabe_kube::kube::runtime::watcher::{Error, Event};
use futures::StreamExt as _;
use support::{Answer, Apiserver, assert_list, mock_cluster, namespace_list, next};
use tokio::time::Instant;

mod support;

#[tokio::test(start_paused = true)]
async fn a_forbidden_watch_relists_only_after_30_seconds() {
    let apiserver = Apiserver::new(
        vec![
            Answer::Status(403, "Forbidden"),
            namespace_list(&["default"], "1"),
        ],
        Vec::new(),
    );
    let cluster = mock_cluster(&apiserver);
    let mut watch = cluster.watch_namespaces();

    assert!(matches!(next(&mut watch).await, Ok(Event::Init)));
    let error = next(&mut watch).await.unwrap_err();
    let failed_at = Instant::now();
    assert!(error.is_forbidden(), "{error}");
    assert!(
        error.to_string().contains("Forbidden by the mock"),
        "{error}"
    );

    tokio::time::sleep(Duration::from_secs(29)).await;
    assert_eq!(apiserver.lists().len(), 1);

    assert_list(&mut watch, &["default"]).await;
    assert_eq!(failed_at.elapsed(), Duration::from_secs(30));
    assert_eq!(apiserver.lists().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_cluster_stops_its_watches() {
    let apiserver = Apiserver::new(vec![namespace_list(&["default"], "1")], Vec::new());
    let cluster = mock_cluster(&apiserver);
    let mut watch = cluster.watch_namespaces();
    assert_list(&mut watch, &["default"]).await;

    drop(cluster);

    // Bounded, so a watch that outlives its handle fails instead of
    // hanging: the paused clock reaches the bound once nothing else runs.
    let ended = tokio::time::timeout(Duration::from_secs(60), watch.next()).await;
    assert!(matches!(ended, Ok(None)), "the watch outlived its handle");
}

/// No kubeconfig names this context, on CI or a dev machine.
const NO_SUCH_CONTEXT: &str = "astrolabe-test-no-such-context";

#[tokio::test]
async fn a_connect_failure_is_the_watch_only_message() {
    let cluster = Cluster::connect(NO_SUCH_CONTEXT);
    let mut watch = cluster.watch_crds();

    let error = watch.next().await.unwrap().unwrap_err();

    assert_eq!(error.kind(), ClusterErrorKind::Connect);
    assert!(watch.next().await.is_none());
}

fn status_box(code: u16) -> Box<Status> {
    Box::new(Status {
        code,
        ..Status::default()
    })
}

#[test]
fn only_403_responses_count_as_rbac_denial() {
    let api = |code| astrolabe_kube::kube::Error::Api(status_box(code));
    let forbidden = |error: Error| ClusterError::from(error).is_forbidden();
    assert!(forbidden(Error::InitialListFailed(api(403))));
    assert!(forbidden(Error::WatchStartFailed(api(403))));
    assert!(forbidden(Error::WatchFailed(api(403))));
    assert!(forbidden(Error::WatchError(status_box(403))));

    assert!(!forbidden(Error::InitialListFailed(api(500))));
    assert!(!forbidden(Error::WatchStartFailed(api(401))));
    assert!(!forbidden(Error::WatchFailed(api(410))));
    assert!(!forbidden(Error::WatchError(status_box(410))));
    assert!(!forbidden(Error::NoResourceVersion));
}

#[test]
fn a_forbidden_error_is_forbidden_and_shows_its_message() {
    let error = ClusterError::forbidden("pods is forbidden");

    assert_eq!(error.kind(), ClusterErrorKind::Forbidden);
    assert!(error.is_forbidden());
    assert_eq!(error.to_string(), "pods is forbidden");
}
