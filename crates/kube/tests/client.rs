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
        r#"apiVersion: v1
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
"#
    )
}

#[tokio::test]
async fn context_selection_uses_kubeconfig_contexts() {
    let kubeconfig = KubeconfigGuard::new(&config("zeta"));
    assert_eq!(
        astrolabe_kube::context_names().unwrap(),
        ["zeta", "alpha", "beta"]
    );

    kubeconfig.replace(&config("missing"));
    astrolabe_kube::client_for_context("alpha").await.unwrap();
}
