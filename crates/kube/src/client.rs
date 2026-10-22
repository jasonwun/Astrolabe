//! Client construction.

/// Build a client from the ambient environment (KUBECONFIG / in-cluster).
///
/// Exec credential plugins (`aws eks get-token`, `kubelogin`, …) are
/// honored, including token refresh. Caveat: the *first* plugin invocation
/// is a blocking subprocess call inside config loading — never call this
/// on the UI thread; run it on a background thread/runtime.
pub async fn default_client() -> kube::Result<kube::Client> {
    kube::Client::try_default().await
}

pub async fn client_for_context(context: &str) -> kube::Result<kube::Client> {
    let options = kube::config::KubeConfigOptions {
        context: Some(context.to_string()),
        ..kube::config::KubeConfigOptions::default()
    };
    let config = kube::Config::from_kubeconfig(&options).await?;
    kube::Client::try_from(config)
}

/// Load context names with the current context first. Call off the UI thread.
pub fn context_names() -> Result<Vec<String>, kube::config::KubeconfigError> {
    let config = kube::config::Kubeconfig::read()?;
    let mut names: Vec<String> = config
        .contexts
        .iter()
        .map(|context| context.name.clone())
        .collect();
    names.sort();
    if let Some(current) = config.current_context
        && let Some(index) = names.iter().position(|name| name == &current)
    {
        names.remove(index);
        names.insert(0, current);
    }
    Ok(names)
}
