//! Bridge between the GPUI main thread and cluster I/O.
//!
//! `astrolabe-kube` is pure tokio-side code; the GPUI executor has no tokio
//! reactor, so a dedicated multi-thread runtime lives here. Each operation
//! runs on that runtime and reports back through a oneshot the view awaits
//! inside `cx.spawn`.

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
