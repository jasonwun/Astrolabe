//! Client construction and the Clusters the kubeconfig names.

use crate::cluster::{ClusterError, ClusterId};

/// Build a client from the ambient environment (KUBECONFIG / in-cluster).
///
/// Exec credential plugins (`aws eks get-token`, `kubelogin`, …) are
/// honored, including token refresh. Caveat: the *first* plugin invocation
/// is a blocking subprocess call inside config loading — never call this
/// on the UI thread; run it on a background thread/runtime.
///
/// # Errors
///
/// A [`kube::Error`] when no kubeconfig or in-cluster config can be
/// inferred, or the client cannot be built from it.
pub async fn default_client() -> kube::Result<kube::Client> {
    kube::Client::try_default().await
}

/// Build a client for `cluster`, whatever the kubeconfig's
/// `current-context` names.
///
/// # Errors
///
/// A [`kube::Error`] when the kubeconfig cannot be read or parsed, has no
/// context for `cluster`, or the client cannot be built from it.
pub(crate) async fn client_for(cluster: &ClusterId) -> kube::Result<kube::Client> {
    let options = kube::config::KubeConfigOptions {
        context: Some(cluster.context().to_owned()),
        ..kube::config::KubeConfigOptions::default()
    };
    let mut config = kube::Config::from_kubeconfig(&options).await?;
    config.default_retry = false;
    Ok(kube::client::ClientBuilder::try_from(config)?
        .with_layer(&BoundedRetryLayer)
        .build())
}

/// Retries 429/503/504 up to 3 times, within about a second.
///
/// kube-client's own retry (`Config::default_retry`) makes 15 attempts that
/// back off to minutes, so a Discovery request to an unavailable
/// `APIService` (503) took about ten minutes to fail (#279). Its policy is
/// not configurable, so the default is turned off and this replaces it.
#[derive(Clone, Copy)]
struct BoundedRetryLayer;

impl<S, B> tower::Layer<S> for BoundedRetryLayer
where
    S: tower::Service<
            http::Request<kube::client::Body>,
            Response = http::Response<B>,
            Error = tower::BoxError,
        > + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Service = tower::retry::Retry<
        kube::client::retry::RetryPolicy,
        tower::buffer::Buffer<http::Request<kube::client::Body>, S::Future>,
    >;

    fn layer(&self, inner: S) -> Self::Service {
        // `Retry` needs a `Clone` service and kube-client's stack is not one.
        let policy = kube::client::retry::RetryPolicy::new(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(500),
            3,
            true,
        )
        .expect("the retry bounds are ordered");
        tower::retry::Retry::new(policy, tower::buffer::Buffer::new(inner, 1024))
    }
}

/// The Clusters the user can open: the kubeconfig's contexts, the current
/// one first and the rest by name. Reads the kubeconfig, which blocks:
/// call off the UI thread.
///
/// # Errors
///
/// An [`Other`](crate::cluster::ClusterErrorKind::Other) [`ClusterError`]
/// when the kubeconfig cannot be found, read or parsed.
pub fn clusters() -> Result<Vec<ClusterId>, ClusterError> {
    let config = kube::config::Kubeconfig::read().map_err(ClusterError::other)?;
    let mut clusters: Vec<ClusterId> = config
        .contexts
        .iter()
        .map(|context| ClusterId::named(context.name.clone()))
        .collect();
    clusters.sort();
    if let Some(current) = config.current_context
        && let Some(index) = clusters
            .iter()
            .position(|cluster| cluster.context() == current)
    {
        let current = clusters.remove(index);
        clusters.insert(0, current);
    }
    Ok(clusters)
}

/// The namespace `kubectl` uses when a context names none.
const DEFAULT_NAMESPACE: &str = "default";

/// The namespace the kubeconfig's context `context` names, or `"default"`
/// when the context names none or does not exist: what `kubectl` starts in.
pub(crate) fn context_namespace(config: &kube::config::Kubeconfig, context: &str) -> String {
    config
        .contexts
        .iter()
        .find(|named| named.name == context)
        .and_then(|named| named.context.as_ref())
        .and_then(|context| context.namespace.clone())
        .unwrap_or_else(|| DEFAULT_NAMESPACE.to_owned())
}

/// The default namespace of `cluster`'s kubeconfig context. Reads the
/// kubeconfig, a small file; a kubeconfig that cannot be read answers
/// `"default"`, and the connect reports the failure.
pub(crate) fn default_namespace_for(cluster: &ClusterId) -> String {
    kube::config::Kubeconfig::read().map_or_else(
        |_| DEFAULT_NAMESPACE.to_owned(),
        |config| context_namespace(&config, cluster.context()),
    )
}

#[cfg(test)]
mod tests {
    use super::context_namespace;

    const KUBECONFIG: &str = r"apiVersion: v1
kind: Config
clusters:
- name: cluster
  cluster:
    server: http://127.0.0.1:8080
contexts:
- name: team
  context:
    cluster: cluster
    user: user
    namespace: team-a
- name: plain
  context:
    cluster: cluster
    user: user
users:
- name: user
  user: {}
";

    fn namespace_of(context: &str) -> String {
        let config = kube::config::Kubeconfig::from_yaml(KUBECONFIG).unwrap();
        context_namespace(&config, context)
    }

    #[test]
    fn a_context_with_a_namespace_starts_in_it() {
        assert_eq!(namespace_of("team"), "team-a");
    }

    #[test]
    fn a_context_without_a_namespace_starts_in_default() {
        assert_eq!(namespace_of("plain"), "default");
        assert_eq!(namespace_of("missing"), "default");
    }
}

#[cfg(test)]
mod retry_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tower::{Layer, Service, ServiceExt};

    use super::BoundedRetryLayer;

    async fn attempts_for(status: u16) -> (usize, u16) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let inner = tower::service_fn(move |_: http::Request<kube::client::Body>| {
            count.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok::<_, tower::BoxError>(
                http::Response::builder()
                    .status(status)
                    .body(String::new())
                    .unwrap(),
            ))
        });
        let mut service = BoundedRetryLayer.layer(inner);
        let request = http::Request::builder()
            .uri("/apis/g/v1")
            .body(kube::client::Body::empty())
            .unwrap();
        let response = service.ready().await.unwrap().call(request).await.unwrap();
        (attempts.load(Ordering::SeqCst), response.status().as_u16())
    }

    #[tokio::test(start_paused = true)]
    async fn a_503_is_retried_three_times_then_returned() {
        assert_eq!(attempts_for(503).await, (4, 503));
    }

    #[tokio::test(start_paused = true)]
    async fn a_404_is_not_retried() {
        assert_eq!(attempts_for(404).await, (1, 404));
    }
}
