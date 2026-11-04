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
    let config = kube::Config::from_kubeconfig(&options).await?;
    kube::Client::try_from(config)
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
    let config =
        kube::config::Kubeconfig::read().map_err(|error| ClusterError::new(error.to_string()))?;
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

/// The namespace the kubeconfig's context `context` names, or `"default"`
/// when the context names none or does not exist: what `kubectl` starts in.
pub(crate) fn context_namespace(config: &kube::config::Kubeconfig, context: &str) -> String {
    config
        .contexts
        .iter()
        .find(|named| named.name == context)
        .and_then(|named| named.context.as_ref())
        .and_then(|context| context.namespace.clone())
        .unwrap_or_else(|| "default".to_owned())
}

/// The default namespace of `cluster`'s kubeconfig context. Reads the
/// kubeconfig, a small file; a kubeconfig that cannot be read answers
/// `"default"`, and the connect reports the failure.
pub(crate) fn default_namespace_for(cluster: &ClusterId) -> String {
    kube::config::Kubeconfig::read().map_or_else(
        |_| "default".to_owned(),
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
