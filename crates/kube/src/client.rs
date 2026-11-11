//! Client construction and the Clusters the kubeconfig names.

use crate::cluster::{ClusterError, ClusterId};
use crate::shell_env::ShellEnv;

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

/// What reading the kubeconfig gave: one connect attempt's only read.
pub(crate) type KubeconfigRead = Result<kube::config::Kubeconfig, kube::config::KubeconfigError>;

/// Read the kubeconfig, which blocks on a small file and, on the first
/// call, on the login shell (#379): call off the UI thread.
pub(crate) fn read_kubeconfig() -> KubeconfigRead {
    read_kubeconfig_in(Some(crate::shell_env::shell_env()))
}

/// Read the kubeconfig without waiting on the login shell: from the
/// location it has captured so far, if any. For the caller's thread, which
/// must not block on it.
pub(crate) fn read_kubeconfig_now() -> KubeconfigRead {
    read_kubeconfig_in(crate::shell_env::captured_shell_env())
}

/// Read the kubeconfig where the process says, or where the login shell
/// `shell` does when the process names none. `KUBECONFIG` merges as
/// kube's own handling does.
fn read_kubeconfig_in(shell: Option<&ShellEnv>) -> KubeconfigRead {
    match shell
        .and_then(ShellEnv::kubeconfig)
        .filter(|_| std::env::var_os("KUBECONFIG").is_none())
    {
        Some(paths) => read_kubeconfig_paths(paths),
        None => kube::config::Kubeconfig::read(),
    }
}

/// Merge the kubeconfig files `paths` lists, like a `KUBECONFIG` value.
fn read_kubeconfig_paths(paths: &str) -> KubeconfigRead {
    let files: Vec<_> = std::env::split_paths(paths)
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    if files.is_empty() {
        return kube::config::Kubeconfig::read();
    }
    files
        .iter()
        .try_fold(kube::config::Kubeconfig::default(), |merged, file| {
            merged.merge(kube::config::Kubeconfig::read_from(file)?)
        })
}

/// Build a client for `cluster` from `kubeconfig`, whatever its
/// `current-context` names. The caller reads it afresh for each connect
/// attempt, so a retry sees a kubeconfig fixed since the last failure.
///
/// # Errors
///
/// A [`Connect`](crate::cluster::ClusterErrorKind::Connect) failure when
/// the kubeconfig could not be read or parsed, has no context for
/// `cluster`, or the client cannot be built from it, as when its
/// credential plugin fails.
pub(crate) async fn client_for(
    cluster: &ClusterId,
    kubeconfig: KubeconfigRead,
    shell: &ShellEnv,
) -> Result<kube::Client, ClusterError> {
    let mut kubeconfig = kubeconfig.map_err(ClusterError::connect_failed)?;
    shell.apply(&mut kubeconfig);
    for exec in kubeconfig
        .auth_infos
        .iter_mut()
        .filter_map(|user| user.auth_info.as_mut()?.exec.as_mut())
    {
        prepare_exec(exec);
    }
    let options = kube::config::KubeConfigOptions {
        context: Some(cluster.context().to_owned()),
        ..kube::config::KubeConfigOptions::default()
    };
    let mut config = kube::Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .map_err(ClusterError::connect_failed)?;
    config.default_retry = false;
    let user = config.auth_info.clone();
    let builder = kube::client::ClientBuilder::try_from(config)
        .map_err(|error| connect_error(&user, error))?;
    Ok(builder.with_layer(&BoundedRetryLayer).build())
}

/// Ready a kubeconfig exec block to run in an app with no terminal.
///
/// It never runs interactively, whatever the kubeconfig says, so its
/// stderr is captured for [`plugin_failure`] instead of going to a
/// terminal nobody reads, and it never waits on a stdin nobody types into.
fn prepare_exec(exec: &mut kube::config::ExecConfig) {
    exec.interactive_mode = Some(kube::config::ExecInteractiveMode::Never);
}

/// A failed client build as a Connect failure, said as
/// [`plugin_failure`] says it when its credential plugin failed.
fn connect_error(user: &kube::config::AuthInfo, error: kube::Error) -> ClusterError {
    match plugin_failure(plugin_program(user), &error) {
        Some(message) => ClusterError::connect(message),
        None => ClusterError::connect_failed(error),
    }
}

/// What a credential plugin failure in `error`'s source chain says: the
/// plugin `program` when known, its exit status and its stderr. The
/// caller keeps no source, as kube-client's error prints the plugin's
/// stdout, which may hold a token.
pub(crate) fn plugin_failure(
    program: Option<&str>,
    error: &(dyn std::error::Error + 'static),
) -> Option<String> {
    let (status, out) =
        std::iter::successors(Some(error), |error| error.source()).find_map(|error| {
            match error.downcast_ref::<kube::client::AuthError>()? {
                kube::client::AuthError::AuthExecRun { status, out, .. } => Some((status, out)),
                _ => None,
            }
        })?;
    let plugin = program.map_or_else(
        || "credential plugin".to_owned(),
        |program| format!("credential plugin \"{program}\""),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr = stderr.trim();
    Some(if stderr.is_empty() {
        format!("{plugin} failed ({status}) and printed nothing on stderr")
    } else {
        format!("{plugin} failed ({status}): {stderr}")
    })
}

/// The program `user`'s credentials come from: its exec plugin's command,
/// or its gcp auth-provider's `cmd-path`.
fn plugin_program(user: &kube::config::AuthInfo) -> Option<&str> {
    if let Some(command) = user.exec.as_ref().and_then(|exec| exec.command.as_deref()) {
        return Some(command);
    }
    user.auth_provider
        .as_ref()?
        .config
        .get("cmd-path")
        .map(String::as_str)
}

/// Retries 429/503/504 up to 3 times, within about a second.
///
/// kube-client's own retry (`Config::default_retry`) makes 15 attempts that
/// back off to minutes, so a Discovery request to an unavailable
/// `APIService` (503) took about ten minutes to fail (#279). Its policy is
/// not configurable, so the default is turned off and this replaces it. See
/// `docs/adr/0003-bounded-client-retry.md` for the costs and when to revisit.
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
    let config = read_kubeconfig().map_err(ClusterError::other)?;
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

/// The default namespace of `cluster`'s kubeconfig context in `kubeconfig`;
/// one that could not be read answers `"default"`, and the connect reports
/// the failure.
pub(crate) fn default_namespace_for(kubeconfig: &KubeconfigRead, cluster: &ClusterId) -> String {
    kubeconfig.as_ref().map_or_else(
        |_| DEFAULT_NAMESPACE.to_owned(),
        |config| context_namespace(config, cluster.context()),
    )
}

#[cfg(test)]
mod tests {
    use super::{context_namespace, read_kubeconfig_paths};
    use crate::shell_env::ShellEnv;
    use std::os::unix::fs::PermissionsExt as _;

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

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("astrolabe-client-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The Finder case: `kubelogin` is bare in the kubeconfig and lives in
    /// a directory this process's `PATH` lacks; only the shell's has it.
    #[tokio::test]
    async fn a_bare_exec_command_resolves_through_the_injected_path() {
        let bin = temp_dir("bin");
        let marker = bin.join("ran");
        let plugin = bin.join("astrolabe-test-plugin-379");
        std::fs::write(
            &plugin,
            format!(
                "#!/bin/sh\ntouch {}\necho '{{\"apiVersion\":\"client.authentication.k8s.io/v1\",\"kind\":\"ExecCredential\",\"status\":{{\"token\":\"t\"}}}}'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&plugin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let yaml = r"apiVersion: v1
kind: Config
clusters:
- name: c
  cluster:
    server: http://127.0.0.1:1
contexts:
- name: ctx
  context: {cluster: c, user: u}
users:
- name: u
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1
      command: astrolabe-test-plugin-379
      interactiveMode: Never
";
        let shell = ShellEnv::parse(
            format!(
                "{m}PATH={}:/usr/bin:/bin\0{m}",
                bin.display(),
                m = crate::shell_env::MARKER
            )
            .as_bytes(),
        );
        let cluster = crate::cluster::ClusterId::named("ctx");
        let read = || kube::config::Kubeconfig::from_yaml(yaml);
        let _ = super::client_for(&cluster, read(), &ShellEnv::default()).await;
        assert!(!marker.exists(), "the plugin ran without the shell's PATH");
        super::client_for(&cluster, read(), &shell).await.unwrap();
        assert!(marker.exists(), "the bare command did not resolve");
    }

    #[test]
    fn a_kubeconfig_location_lists_files_that_merge_first_wins() {
        let dir = temp_dir("kubeconfigs");
        let write = |name: &str, current: &str, context: &str| {
            let path = dir.join(name);
            std::fs::write(
                &path,
                format!(
                    "apiVersion: v1\nkind: Config\ncurrent-context: {current}\ncontexts:\n- name: {context}\n  context: {{cluster: c, user: u}}\n"
                ),
            )
            .unwrap();
            path
        };
        let first = write("a.yaml", "one", "one");
        let second = write("b.yaml", "two", "two");
        let paths = std::env::join_paths([&first, &second]).unwrap();
        let config = read_kubeconfig_paths(paths.to_str().unwrap()).unwrap();
        assert_eq!(config.current_context.as_deref(), Some("one"));
        let names: Vec<_> = config.contexts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["one", "two"]);
        assert!(read_kubeconfig_paths(&format!("{}x", first.display())).is_err());
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
