//! The relist after a watch error, with its log trail. It is the only test
//! in this binary: a test asserting a log line is the only one in its
//! binary that reaches the line's callsite (`CODING_STANDARDS.md`).

use std::time::Duration;

use astrolabe_kube::cluster::ClusterErrorKind;
use support::{Apiserver, Logs, assert_list, failing_watch, mock_cluster, namespace_list, next};
use tokio::time::Instant;

mod support;

#[tokio::test(start_paused = true)]
async fn a_watch_error_forces_a_complete_relist_a_second_later() {
    let logs = Logs::default();
    logs.install();
    let apiserver = Apiserver::new(
        vec![
            namespace_list(&["default"], "1"),
            namespace_list(&["kube-system"], "2"),
        ],
        vec![failing_watch()],
    );
    let cluster = mock_cluster(&apiserver);
    let mut watch = cluster.watch_namespaces();

    // 1. Initial list/watch establishes a snapshot.
    assert_list(&mut watch, &["default"]).await;

    // 2. The failing watch arrives as an error that is not a 403.
    let error = next(&mut watch).await.unwrap_err();
    let failed_at = Instant::now();
    assert_eq!(error.kind(), ClusterErrorKind::Other);
    assert!(
        error.to_string().contains("InternalError by the mock"),
        "{error}"
    );

    // 3./4. Recovery is a fresh LIST, not a silent watch resumption: the
    // relist completes from the LIST alone and replaces the snapshot.
    assert_list(&mut watch, &["kube-system"]).await;
    assert_eq!(failed_at.elapsed(), Duration::from_secs(1));
    assert_eq!(apiserver.lists().len(), 2);

    // 5. The new watch is anchored to the fresh list.
    tokio::time::sleep(Duration::from_millis(1)).await;
    let watches = apiserver.watches();
    assert_eq!(watches.len(), 2, "{watches:?}");
    assert!(watches[1].contains("resourceVersion=2"), "{watches:?}");

    // The RUST_LOG-driven event trail covers the failure and the recovery
    // without ever logging a resource payload.
    let text = logs.text();
    assert!(
        text.contains("namespace watch failed"),
        "missing watch failure log:\n{text}"
    );
    assert!(
        text.contains("restarting namespace watch"),
        "missing recovery attempt log:\n{text}"
    );
    // kube-rs emits its own watcher/backoff events at debug under the
    // kube_runtime target (verified against kube-runtime 4.2).
    assert!(
        text.contains("kube_runtime"),
        "missing kube-rs watcher/backoff events:\n{text}"
    );
    assert!(
        !text.contains("NamespaceList"),
        "log contains a resource payload:\n{text}"
    );
}
