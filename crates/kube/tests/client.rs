use astrolabe_kube::cluster::{Cluster, ClusterErrorKind, ClusterId};
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::{env, ffi::OsString, fs};

struct KubeconfigGuard {
    path: PathBuf,
    previous: Option<OsString>,
}

impl KubeconfigGuard {
    fn new(contents: &str) -> Self {
        let path =
            env::temp_dir().join(format!("astrolabe-kube-client-{}.yaml", std::process::id()));
        fs::write(&path, contents).unwrap();
        let previous = env::var_os("KUBECONFIG");
        unsafe { env::set_var("KUBECONFIG", &path) };
        Self { path, previous }
    }

    fn replace(&self, contents: &str) {
        fs::write(&self.path, contents).unwrap();
    }
}

impl Drop for KubeconfigGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            unsafe { env::set_var("KUBECONFIG", previous) };
        } else {
            unsafe { env::remove_var("KUBECONFIG") };
        }
        fs::remove_file(&self.path).ok();
    }
}

fn config(current_context: &str) -> String {
    format!(
        r"apiVersion: v1
kind: Config
current-context: {current_context}
clusters:
- name: cluster
  cluster:
    server: http://127.0.0.1:8080
contexts:
- name: zeta
  context:
    cluster: cluster
    user: user
- name: alpha
  context:
    cluster: cluster
    user: user
- name: beta
  context:
    cluster: cluster
    user: user
    namespace: team-a
users:
- name: user
  user: {{}}
"
    )
}

/// A credential plugin named `name` that prints `stderr` there, a token
/// on stdout, and exits 1.
fn failing_plugin(name: &str, stderr: &str) -> PathBuf {
    let path = env::temp_dir().join(format!(
        "astrolabe-kube-plugin-{}-{name}.sh",
        std::process::id()
    ));
    let script = format!("#!/bin/sh\nprintf '{stderr}' >&2\necho secret-token\nexit 1\n");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A kubeconfig with one context per `(name, user)`, each user given as
/// its YAML body.
fn users_config(users: &[(&str, String)]) -> String {
    let mut contexts = String::new();
    let mut named_users = String::new();
    for (name, user) in users {
        write!(
            contexts,
            "- name: {name}\n  context:\n    cluster: cluster\n    user: {name}\n"
        )
        .unwrap();
        write!(named_users, "- name: {name}\n  user:\n{user}").unwrap();
    }
    format!(
        "apiVersion: v1\nkind: Config\nclusters:\n- name: cluster\n  cluster:\n    server: http://127.0.0.1:8080\ncontexts:\n{contexts}users:\n{named_users}"
    )
}

/// A credential plugin that gives an expired token the first time it
/// runs, so the client runs it again on its first request, and fails
/// like [`failing_plugin`] from then on.
fn expiring_plugin() -> PathBuf {
    let path = env::temp_dir().join(format!(
        "astrolabe-kube-plugin-{}-expiring.sh",
        std::process::id()
    ));
    let script = r#"#!/bin/sh
if [ -e "$0.ran" ]; then printf 'refresh token expired' >&2; echo secret-token; exit 1; fi
touch "$0.ran"
echo '{"apiVersion":"client.authentication.k8s.io/v1beta1","kind":"ExecCredential","status":{"token":"first","expirationTimestamp":"2000-01-01T00:00:00Z"}}'
"#;
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A user whose exec plugin is `plugin`, with `extra` exec fields.
fn exec_user(plugin: &Path, extra: &str) -> String {
    format!(
        "    exec:\n      apiVersion: client.authentication.k8s.io/v1beta1\n      command: {}\n{extra}",
        plugin.display()
    )
}

async fn discovery_error(context: &str) -> astrolabe_kube::cluster::ClusterError {
    Cluster::connect(&ClusterId::named(context))
        .discover()
        .await
        .unwrap_err()
}

fn names(clusters: &[ClusterId]) -> Vec<String> {
    clusters.iter().map(ToString::to_string).collect()
}

/// One test, as all steps point `KUBECONFIG` at a file: tests in one
/// binary run in parallel and share the environment.
#[tokio::test]
async fn clusters_are_the_kubeconfig_contexts_current_first() {
    let kubeconfig = KubeconfigGuard::new(&config("zeta"));
    let clusters = astrolabe_kube::clusters().unwrap();
    assert_eq!(names(&clusters), ["zeta", "alpha", "beta"]);

    // A listed Cluster connects whatever `current-context` names.
    kubeconfig.replace(&config("missing"));
    assert_eq!(
        names(&astrolabe_kube::clusters().unwrap()),
        ["alpha", "beta", "zeta"]
    );
    let alpha = clusters
        .iter()
        .find(|cluster| cluster.name() == "alpha")
        .unwrap();
    assert_eq!(Cluster::connect(alpha).default_namespace(), "default");
    let beta = clusters
        .iter()
        .find(|cluster| cluster.name() == "beta")
        .unwrap();
    assert_eq!(Cluster::connect(beta).default_namespace(), "team-a");
    let error = Cluster::connect(alpha).discover().await.unwrap_err();
    assert_ne!(error.kind(), ClusterErrorKind::Connect, "{error}");

    // A later attempt reads the kubeconfig afresh, so a retry sees a
    // context added since the first attempt failed (#278, for #277).
    let ghost = ClusterId::named("ghost");
    let cluster = Cluster::connect(&ghost);
    let error = cluster.discover().await.unwrap_err();
    assert_eq!(error.kind(), ClusterErrorKind::Connect, "{error}");
    kubeconfig.replace(&config("zeta").replace(
        "contexts:\n",
        "contexts:\n- name: ghost\n  context:\n    cluster: cluster\n    user: user\n",
    ));
    cluster.reconnect();
    let error = cluster.discover().await.unwrap_err();
    assert_ne!(error.kind(), ClusterErrorKind::Connect, "{error}");

    // A failed credential plugin says what it printed on stderr, whatever
    // the kubeconfig's interactiveMode, and never its stdout (#378).
    let expired = failing_plugin("expired", "\\n  login expired, run sso login\\n");
    let silent = failing_plugin("silent", "");
    kubeconfig.replace(&users_config(&[
        ("plain", exec_user(&expired, "")),
        (
            "always",
            exec_user(&expired, "      interactiveMode: Always\n"),
        ),
        ("silent", exec_user(&silent, "")),
        (
            "gcp",
            format!(
                "    auth-provider:\n      name: gcp\n      config:\n        cmd-path: {}\n",
                expired.display()
            ),
        ),
    ]));
    let failed = format!(
        "credential plugin \"{}\" failed (exit status: 1): login expired, run sso login",
        expired.display()
    );
    for context in ["plain", "always", "gcp"] {
        let error = discovery_error(context).await;
        assert_eq!(
            error.kind(),
            ClusterErrorKind::Connect,
            "{context}: {error}"
        );
        assert_eq!(error.to_string(), failed, "{context}");
    }
    let error = discovery_error("silent").await;
    assert_eq!(
        error.to_string(),
        format!(
            "credential plugin \"{}\" failed (exit status: 1) and printed nothing on stderr",
            silent.display()
        )
    );
    // Nothing in the source chain prints the plugin's stdout either.
    assert!(
        std::error::Error::source(&error).is_none(),
        "{:?}",
        std::error::Error::source(&error)
    );

    // So does one that fails when the client runs it again for a fresh
    // token, after the client was built.
    let expiring = expiring_plugin();
    kubeconfig.replace(&users_config(&[("expiring", exec_user(&expiring, ""))]));
    let error = discovery_error("expiring").await;
    assert_eq!(
        error.to_string(),
        "credential plugin failed (exit status: 1): refresh token expired"
    );
    assert!(std::error::Error::source(&error).is_none());

    fs::remove_file(expired).ok();
    fs::remove_file(silent).ok();
    fs::remove_file(&expiring).ok();
    fs::remove_file(expiring.with_extension("sh.ran")).ok();
}
