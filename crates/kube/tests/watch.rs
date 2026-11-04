//! Watches through the `Cluster` handle over a mock apiserver, on a paused
//! tokio clock: the 1 s and 30 s retries pass at once.

use std::time::Duration;

use astrolabe_kube::cluster::{Cluster, ClusterError, ClusterErrorKind, ClusterId};
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
    // A later watch prunes finished ones and must keep the running first.
    let mut later = cluster.watch_namespaces();

    drop(cluster);

    // Bounded, so a watch that outlives its handle fails instead of
    // hanging: the paused clock reaches the bound once nothing else runs.
    for watch in [&mut watch, &mut later] {
        let ended = tokio::time::timeout(Duration::from_secs(60), watch.next()).await;
        assert!(matches!(ended, Ok(None)), "the watch outlived its handle");
    }
}

/// No kubeconfig names this context, on CI or a dev machine.
fn no_such_cluster() -> ClusterId {
    ClusterId::named("astrolabe-test-no-such-context")
}

#[tokio::test]
async fn a_connect_failure_is_the_watch_only_message() {
    let cluster = Cluster::connect(&no_such_cluster());
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

/// Read messages until the next error, and when it arrived.
async fn next_error(
    watch: &mut futures::channel::mpsc::Receiver<astrolabe_kube::watch::NamespaceWatchMessage>,
) -> Instant {
    loop {
        if next(watch).await.is_err() {
            return Instant::now();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_relist_wait_doubles_up_to_30_seconds() {
    let apiserver = Apiserver::new(
        (0..8)
            .map(|_| Answer::Status(500, "InternalError"))
            .collect(),
        Vec::new(),
    );
    let cluster = mock_cluster(&apiserver);
    let mut watch = cluster.watch_namespaces();

    let mut failures = vec![next_error(&mut watch).await];
    for _ in 1..8 {
        failures.push(next_error(&mut watch).await);
    }

    let waits: Vec<u64> = failures
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).as_secs())
        .collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
}

#[tokio::test(start_paused = true)]
async fn a_completed_list_resets_the_relist_wait() {
    let apiserver = Apiserver::new(
        vec![
            Answer::Status(500, "InternalError"),
            Answer::Status(500, "InternalError"),
            namespace_list(&["default"], "1"),
            namespace_list(&["kube-system"], "2"),
        ],
        vec![support::failing_watch()],
    );
    let cluster = mock_cluster(&apiserver);
    let mut watch = cluster.watch_namespaces();

    // Two failed LISTs: the waits so far are 1 s and 2 s, the next would
    // be 4 s.
    next_error(&mut watch).await;
    next_error(&mut watch).await;
    // The third LIST completes, then its watch fails.
    assert_list(&mut watch, &["default"]).await;
    let failed_at = next_error(&mut watch).await;

    assert_list(&mut watch, &["kube-system"]).await;
    assert_eq!(failed_at.elapsed(), Duration::from_secs(1));
}
