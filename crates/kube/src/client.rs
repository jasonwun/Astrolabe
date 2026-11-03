//! Client construction and the Clusters the kubeconfig names.

use crate::cluster::ClusterError;

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

/// A Cluster the app can open, named without saying where it comes from.
///
/// Today every Cluster is a kubeconfig context and the id wraps its name;
/// a second source changes only this type's inside and this crate. Outside
/// tests, ids come from [`clusters`]. It orders by name, so that a list
/// keyed by it is stable.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClusterId {
    context: String,
}

impl ClusterId {
    /// The Cluster that kubeconfig context `name` reaches, whether or not
    /// the kubeconfig has it: what tests and scripted clusters name a
    /// Cluster with. The app takes its ids from [`clusters`].
    pub fn kubeconfig_context(name: impl Into<String>) -> Self {
        Self {
            context: name.into(),
        }
    }

    /// The name the user knows the Cluster by.
    pub fn name(&self) -> &str {
        &self.context
    }
}

/// The Cluster's name.
impl std::fmt::Display for ClusterId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.context)
    }
}

/// Build a client for `cluster`, whatever the kubeconfig's
/// `current-context` names.
///
/// # Errors
///
/// A [`kube::Error`] when the kubeconfig cannot be read or parsed, has no
/// context for `cluster`, or the client cannot be built from it.
pub async fn client_for(cluster: &ClusterId) -> kube::Result<kube::Client> {
    let options = kube::config::KubeConfigOptions {
        context: Some(cluster.context.clone()),
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
        .map(|context| ClusterId::kubeconfig_context(context.name.clone()))
        .collect();
    clusters.sort();
    if let Some(current) = config.current_context
        && let Some(index) = clusters
            .iter()
            .position(|cluster| cluster.context == current)
    {
        let current = clusters.remove(index);
        clusters.insert(0, current);
    }
    Ok(clusters)
}
