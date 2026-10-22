//! Bridge between the GPUI main thread and cluster I/O.
//!
//! `astrolabe-kube` is pure tokio-side code; the GPUI executor has no tokio
//! reactor, so a dedicated multi-thread runtime lives here. One-shot operations
//! report through a oneshot; resident watchers use the returned task handle.

use std::sync::OnceLock;

use tokio::sync::oneshot;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().expect("failed to create tokio runtime"))
}

/// Run `future` on the cluster runtime; the receiver resolves with its
/// output. Safe to await from a GPUI task.
pub fn spawn_cluster<R>(
    future: impl std::future::Future<Output = R> + Send + 'static,
) -> oneshot::Receiver<R>
where
    R: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    runtime().spawn(async move {
        let _ = tx.send(future.await);
    });
    rx
}

pub(crate) fn spawn_cluster_task(
    future: impl std::future::Future<Output = ()> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    runtime().spawn(future)
}
