use astrolabe_kube::cluster::{Cluster, ClusterErrorKind, ClusterId};
use std::{env, ffi::OsString, fs, path::PathBuf};

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
users:
- name: user
  user: {{}}
"
    )
}

fn names(clusters: &[ClusterId]) -> Vec<String> {
    clusters.iter().map(ToString::to_string).collect()
}

/// One test, as both steps point `KUBECONFIG` at a file: tests in one
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
    let error = Cluster::connect(alpha).discover().await.unwrap_err();
    assert_ne!(error.kind(), ClusterErrorKind::Connect, "{error}");
}
