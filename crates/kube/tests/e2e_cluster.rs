//! End-to-end verification against a real cluster: the #21 apply loop and
//! the #17 workload operations (scale, restart, logs, exec, port-forward)
//! and the #18 Service port-forward.
//!
//! Requires a reachable cluster via the ambient kubeconfig (the dev loop
//! uses `kind-astrolabe-dev`). All tests are `#[ignore]`d so the default
//! suite stays hermetic; run them with:
//!
//! ```sh
//! cargo test -p astrolabe-kube --test e2e_cluster -- --ignored --test-threads=1
//! ```

use std::time::Duration;

use astrolabe_kube::apply::{ApplyError, Location, apply_documents, precheck_documents};
use astrolabe_kube::k8s_openapi::api::apps::v1::Deployment;
use astrolabe_kube::k8s_openapi::api::core::v1::{Namespace, Pod};
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube;
use astrolabe_kube::kube::api::{Api, DeleteParams, ListParams, PostParams};
use astrolabe_kube::kube::core::{DynamicObject, GroupVersionKind};
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_kube::kube::runtime::watcher;
use astrolabe_kube::pod::{self, ExecInput};
use astrolabe_kube::{default_client, resource, service};
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Unique namespace per test run; deleted on drop-best-effort at the end.
fn unique_ns() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("e2e-{nanos:x}")
}

fn ns_doc(ns: &str) -> String {
    format!("apiVersion: v1\nkind: Namespace\nmetadata:\n  name: {ns}\n")
}

fn configmap_doc(ns: &str, name: &str, value: &str) -> String {
    format!(
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {name}\n  namespace: {ns}\ndata:\n  value: {value}\n"
    )
}

fn deployment_doc(ns: &str, image_pull_policy: &str) -> String {
    format!(
        "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: nginx
  namespace: {ns}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: nginx
  template:
    metadata:
      labels:
        app: nginx
    spec:
      containers:
        - name: nginx
          image: nginx:1.27
          imagePullPolicy: {image_pull_policy}
"
    )
}

fn join_docs(docs: &[String]) -> String {
    docs.join("---\n")
}

/// 1-based line number of the first line containing `needle`.
fn line_of(snapshot: &str, needle: &str) -> usize {
    snapshot
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in snapshot"))
        + 1
}

async fn delete_ns(client: &kube::Client, ns: &str) {
    let gvk = GroupVersionKind::gvk("", "v1", "Namespace");
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &ApiResource::from_gvk(&gvk));
    let _ = api.delete(ns, &DeleteParams::default()).await;
}

/// Namespaces emit a complete initial list followed by raw live changes.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn namespace_watch_lists_then_streams_create_and_delete() {
    let contexts = astrolabe_kube::context_names().unwrap();
    assert!(!contexts.is_empty(), "kubeconfig must contain a context");
    let client = astrolabe_kube::client_for_context(&contexts[0])
        .await
        .unwrap();
    let events = astrolabe_kube::watch::namespaces(&client);
    futures::pin_mut!(events);

    tokio::time::timeout(Duration::from_secs(20), async {
        let mut saw_init = false;
        let mut saw_init_apply = false;
        loop {
            match events
                .next()
                .await
                .expect("watch ended during initial list")
                .unwrap()
            {
                watcher::Event::Init => saw_init = true,
                watcher::Event::InitApply(_) => saw_init_apply = true,
                watcher::Event::InitDone => {
                    assert!(saw_init, "initial list must start with Init");
                    assert!(saw_init_apply, "initial list must contain objects");
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for the initial namespace list");

    let api = Api::<Namespace>::all(client.clone());
    let ns = unique_ns();
    let namespace: Namespace = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": { "name": ns },
    }))
    .unwrap();
    api.create(&PostParams::default(), &namespace)
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let watcher::Event::Apply(namespace) = events
                .next()
                .await
                .expect("watch ended before create")
                .unwrap()
                && namespace.metadata.name.as_deref() == Some(ns.as_str())
            {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for the namespace create event");

    api.delete(&ns, &DeleteParams::default()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let watcher::Event::Delete(namespace) = events
                .next()
                .await
                .expect("watch ended before delete")
                .unwrap()
                && namespace.metadata.name.as_deref() == Some(ns.as_str())
            {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for the namespace delete event");
}

/// Happy path: a multi-doc buffer (Namespace + ConfigMap + Deployment)
/// applies cleanly, outcomes arrive in send order, and the objects exist.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn apply_multi_doc_happy_path() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    let snapshot = join_docs(&[
        ns_doc(&ns),
        configmap_doc(&ns, "cm", "hello"),
        deployment_doc(&ns, "IfNotPresent"),
    ]);

    let report = apply_documents(&client, &snapshot, false).await.unwrap();
    assert!(report.ok, "all docs should apply: {:?}", report.outcomes);
    assert_eq!(report.outcomes.len(), 3);
    assert_eq!(report.outcomes[0].doc, format!("v1 Namespace/{ns}"));
    assert_eq!(report.outcomes[1].doc, "v1 ConfigMap/cm");
    assert_eq!(report.outcomes[2].doc, "apps/v1 Deployment/nginx");
    for (i, o) in report.outcomes.iter().enumerate() {
        assert_eq!(o.doc_index, i);
        assert!(o.result.is_ok(), "doc {i}: {:?}", o.result);
    }

    let gvk = GroupVersionKind::gvk("", "v1", "ConfigMap");
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &ns, &ApiResource::from_gvk(&gvk));
    api.get("cm").await.expect("ConfigMap should exist");

    delete_ns(&client, &ns).await;
}

/// 422: an invalid enum value produces a Validation error whose cause maps
/// to the exact buffer line of the offending field, and the healthy docs in
/// the same buffer still apply (allSettled).
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn apply_422_maps_cause_to_field_line() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    let snapshot = join_docs(&[
        ns_doc(&ns),
        configmap_doc(&ns, "cm", "hello"),
        deployment_doc(&ns, "NotARealPolicy"),
    ]);

    let report = apply_documents(&client, &snapshot, false).await.unwrap();
    assert!(!report.ok);
    assert!(report.outcomes[0].result.is_ok(), "Namespace applies");
    assert!(report.outcomes[1].result.is_ok(), "ConfigMap applies");

    let err = report.outcomes[2]
        .result
        .as_ref()
        .expect_err("Deployment must fail");
    let ApplyError::Validation(causes) = &err.error else {
        panic!("expected Validation, got {:?}", err.error);
    };
    let cause = causes
        .iter()
        .find(|c| c.raw_field.contains("imagePullPolicy"))
        .expect("a cause should name imagePullPolicy");
    assert!(cause.path.len() > 1, "path should parse: {cause:?}");

    let located = err
        .causes
        .iter()
        .find(|lc| lc.cause.raw_field.contains("imagePullPolicy"))
        .expect("cause should be located");
    assert_eq!(
        located.located.location,
        Location::Exact {
            line: line_of(&snapshot, "imagePullPolicy")
        },
        "cause should land on the imagePullPolicy line"
    );

    delete_ns(&client, &ns).await;
}

/// 409: a field owned by another manager conflicts; Force Apply wins.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn apply_409_then_force_apply() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    // Set up: namespace + ConfigMap owned by a *different* manager.
    let setup = join_docs(&[ns_doc(&ns), configmap_doc(&ns, "cm", "owned-by-other")]);
    let report = apply_documents(&client, &setup, false).await.unwrap();
    assert!(report.ok);

    // Re-own data.value under a foreign manager.
    let gvk = GroupVersionKind::gvk("", "v1", "ConfigMap");
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &ns, &ApiResource::from_gvk(&gvk));
    let foreign = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": "cm", "namespace": ns },
        "data": { "value": "foreign" },
    });
    api.patch(
        "cm",
        &kube::api::PatchParams::apply("e2e-foreign").force(),
        &kube::api::Patch::Apply(foreign),
    )
    .await
    .unwrap();

    // Our manager now conflicts on data.value.
    let snapshot = join_docs(&[configmap_doc(&ns, "cm", "ours")]);
    let report = apply_documents(&client, &snapshot, false).await.unwrap();
    assert!(!report.ok);
    let err = report.outcomes[0]
        .result
        .as_ref()
        .expect_err("apply must conflict");
    let ApplyError::Conflict(managers) = &err.error else {
        panic!("expected Conflict, got {:?}", err.error);
    };
    let conflict = managers
        .iter()
        .find(|m| m.manager == "e2e-foreign")
        .expect("conflict should name the foreign manager");
    assert!(
        conflict
            .causes
            .iter()
            .any(|c| c.raw_field.contains("value")),
        "conflict should name data.value: {conflict:?}"
    );
    let located = err
        .causes
        .iter()
        .find(|lc| lc.cause.raw_field.contains("value"))
        .expect("conflict cause should be located");
    assert_eq!(
        located.located.location,
        Location::Exact {
            line: line_of(&snapshot, "value:")
        }
    );

    // Force Apply takes ownership and succeeds.
    let report = apply_documents(&client, &snapshot, true).await.unwrap();
    assert!(
        report.ok,
        "force apply should succeed: {:?}",
        report.outcomes
    );
    let cm = api.get("cm").await.unwrap();
    assert_eq!(
        cm.data["data"]["value"].as_str().unwrap(),
        "ours",
        "force apply should win the field"
    );

    delete_ns(&client, &ns).await;
}

/// Unknown fields under SSA fail BOTH the dry-run precheck and the real
/// apply with a typed-patch schema error (verified against kind v1.32 —
/// the strict-decoding 422 path assumed by #15 never runs for apply
/// requests). The cause must be parsed, hard (not a warning), and located
/// on the offending line.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn unknown_field_fails_precheck_and_apply_with_located_cause() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    let mut cm = configmap_doc(&ns, "cm", "hello");
    cm.push_str("unknownField: oops\n");
    let snapshot = join_docs(&[ns_doc(&ns), cm]);

    for (label, report) in [
        (
            "precheck",
            precheck_documents(&client, &snapshot).await.unwrap(),
        ),
        (
            "apply",
            apply_documents(&client, &snapshot, false).await.unwrap(),
        ),
    ] {
        assert!(!report.ok, "{label} should flag the unknown field");
        assert!(
            report.outcomes[0].result.is_ok(),
            "{label}: Namespace doc is fine"
        );
        let err = report.outcomes[1]
            .result
            .as_ref()
            .err()
            .unwrap_or_else(|| panic!("{label}: ConfigMap doc must fail"));
        let ApplyError::Validation(causes) = &err.error else {
            panic!("{label}: expected Validation, got {:?}", err.error);
        };
        let cause = causes
            .iter()
            .find(|c| c.raw_field.contains("unknownField"))
            .unwrap_or_else(|| panic!("{label}: a cause should name unknownField: {causes:?}"));
        assert!(
            !cause.is_strict_decoding(),
            "{label}: typed-patch failure is a hard error, not a warning"
        );

        let located = err
            .causes
            .iter()
            .find(|lc| lc.cause.raw_field.contains("unknownField"))
            .unwrap_or_else(|| panic!("{label}: cause should be located"));
        assert_eq!(
            located.located.location,
            Location::Exact {
                line: line_of(&snapshot, "unknownField")
            },
            "{label}: cause should land on the unknownField line"
        );
    }

    delete_ns(&client, &ns).await;
}

/// The #16 CRD pipeline end to end: a fresh CRD shows up in the CRD watch,
/// discovery serves its preferred version once established, the catalog
/// resolves it, a per-namespace instance watch streams it, and delete
/// removes it.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn crd_kind_is_discovered_watched_and_deleted() {
    use astrolabe_kube::crd::{KindResource, catalog};
    use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;

    let client = default_client().await.unwrap();
    let ns = unique_ns();
    let group = format!("{ns}.astrolabe.dev");
    let crd_name = format!("widgets.{group}");
    let crd_doc = format!(
        r#"apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: {crd_name}
spec:
  group: {group}
  scope: Namespaced
  names: {{kind: Widget, plural: widgets, singular: widget}}
  versions:
  - name: v1
    served: true
    storage: true
    schema:
      openAPIV3Schema:
        type: object
        x-kubernetes-preserve-unknown-fields: true
    additionalPrinterColumns:
    - {{name: Size, type: string, jsonPath: .spec.size}}
"#
    );
    let report = apply_documents(&client, &join_docs(&[ns_doc(&ns), crd_doc]), false)
        .await
        .unwrap();
    assert!(report.ok, "{:?}", report.outcomes);

    let crds: Api<CustomResourceDefinition> = Api::all(client.clone());
    let crd = crds.get(&crd_name).await.unwrap();

    // Discovery serves the kind only once the CRD is established.
    let served = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let served = astrolabe_kube::discovery::discover(&client).await.unwrap();
            if served.resolve(&group, "v1", "Widget").is_some() {
                break served;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("CRD never became discoverable");
    assert!(served.resolve("", "v1", "Namespace").is_some());

    let groups = catalog(&[&crd], Some(&served));
    let kind = &groups[0].kinds[0];
    assert!(kind.namespaced);
    assert_eq!(kind.columns[0].name, "Size");
    let KindResource::Served(resource) = &kind.resource else {
        panic!("Widget not served: {:?}", kind.resource);
    };
    assert_eq!(resource.plural, "widgets");

    let events = astrolabe_kube::watch::objects(&client, resource, Some(&ns));
    futures::pin_mut!(events);
    let widget = format!(
        "apiVersion: {group}/v1\nkind: Widget\nmetadata:\n  name: w1\n  namespace: {ns}\nspec:\n  size: large\n"
    );
    assert!(apply_documents(&client, &widget, false).await.unwrap().ok);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.next().await.expect("watch ended").unwrap() {
                watcher::Event::Apply(object) | watcher::Event::InitApply(object)
                    if object.metadata.name.as_deref() == Some("w1") =>
                {
                    assert_eq!(object.data["spec"]["size"], "large");
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for the Widget");

    astrolabe_kube::resource::delete(&client, resource, Some(&ns), "w1")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let watcher::Event::Delete(object) =
                events.next().await.expect("watch ended").unwrap()
                && object.metadata.name.as_deref() == Some("w1")
            {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for the Widget delete");

    let _ = crds.delete(&crd_name, &DeleteParams::default()).await;
    delete_ns(&client, &ns).await;
}

/// RBAC denial is told apart from other watch failures (#16 empty state):
/// impersonating a user with no permissions, the CRD list fails with 403.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn crd_watch_without_rbac_reports_forbidden() {
    let mut config = kube::Config::infer().await.unwrap();
    config.auth_info.impersonate = Some("astrolabe-e2e-nobody".to_string());
    let client = kube::Client::try_from(config).unwrap();
    let events = astrolabe_kube::watch::custom_resource_definitions(&client);
    futures::pin_mut!(events);
    // The watcher announces `Init` before its LIST; the LIST then fails.
    let error = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.next().await.expect("watch ended") {
                Ok(watcher::Event::Init) => {}
                Ok(event) => panic!("an unprivileged user must not list CRDs: {event:?}"),
                Err(error) => break error,
            }
        }
    })
    .await
    .expect("timed out waiting for the CRD list");
    assert!(astrolabe_kube::watch::is_forbidden(&error), "{error}");
}

/// Applies the nginx Deployment in a fresh namespace and waits until one of
/// its Pods is Running and Ready; returns that Pod's name.
async fn running_nginx(client: &kube::Client, ns: &str) -> String {
    let snapshot = join_docs(&[ns_doc(ns), deployment_doc(ns, "IfNotPresent")]);
    let report = apply_documents(client, &snapshot, false).await.unwrap();
    assert!(report.ok, "setup should apply: {:?}", report.outcomes);
    ready_pod(client, ns, None).await
}

/// Polls for a Ready nginx Pod whose name differs from `other_than`.
async fn ready_pod(client: &kube::Client, ns: &str, other_than: Option<&str>) -> String {
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let params = ListParams::default().labels("app=nginx");
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            for pod in pods.list(&params).await.unwrap() {
                let name = pod.metadata.name.clone().unwrap();
                let ready = pod
                    .status
                    .as_ref()
                    .and_then(|s| s.conditions.as_ref())
                    .is_some_and(|c| c.iter().any(|c| c.type_ == "Ready" && c.status == "True"));
                let terminating = pod.metadata.deletion_timestamp.is_some();
                if ready && !terminating && other_than != Some(name.as_str()) {
                    return name;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("timed out waiting for a Ready nginx Pod")
}

/// Scale sets the Deployment's replicas; rollout restart stamps the template
/// and the Deployment replaces its Pod.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn scale_and_rollout_restart_a_deployment() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    let first = running_nginx(&client, &ns).await;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &ns);

    resource::scale(&client, &ns, "nginx", 2).await.unwrap();
    let scaled = deployments.get("nginx").await.unwrap();
    assert_eq!(scaled.spec.unwrap().replicas, Some(2));
    resource::scale(&client, &ns, "nginx", 1).await.unwrap();

    let at: Timestamp = "2026-01-02T03:04:05Z".parse().unwrap();
    resource::rollout_restart(&client, &ns, "nginx", at)
        .await
        .unwrap();
    let restarted = deployments.get("nginx").await.unwrap();
    let annotations = restarted
        .spec
        .unwrap()
        .template
        .metadata
        .unwrap()
        .annotations
        .unwrap();
    assert_eq!(
        annotations["kubectl.kubernetes.io/restartedAt"],
        "2026-01-02T03:04:05Z"
    );
    ready_pod(&client, &ns, Some(&first)).await;

    delete_ns(&client, &ns).await;
}

/// Logs follow the container's stdout; exec runs a shell over a TTY and
/// reports its exit code; port-forward carries an HTTP request to nginx.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn pod_logs_exec_and_port_forward() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    let pod_name = running_nginx(&client, &ns).await;

    let logs = pod::logs(&client, &ns, &pod_name, "nginx").await.unwrap();
    futures::pin_mut!(logs);
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains("ready for start up") {
            seen.extend(logs.next().await.expect("log stream ended").unwrap());
        }
    })
    .await
    .expect("timed out waiting for nginx's start-up log line");

    let (input, input_rx) = futures::channel::mpsc::unbounded();
    input
        .unbounded_send(ExecInput::Resize { cols: 80, rows: 24 })
        .unwrap();
    input
        .unbounded_send(ExecInput::Bytes(b"echo e2e-$((40 + 2)); exit 3\n".to_vec()))
        .unwrap();
    let mut output = Vec::new();
    let code = tokio::time::timeout(
        Duration::from_secs(20),
        pod::exec(
            &client,
            &ns,
            &pod_name,
            "nginx",
            &["sh".to_string()],
            input_rx,
            |bytes| output.extend(bytes),
        ),
    )
    .await
    .expect("timed out waiting for the shell to exit")
    .unwrap();
    assert_eq!(code, 3);
    assert!(
        String::from_utf8_lossy(&output).contains("e2e-42"),
        "shell output: {:?}",
        String::from_utf8_lossy(&output)
    );
    drop(input);

    let listener = pod::bind_local(0).await.unwrap();
    let local = listener.local_addr().unwrap();
    let forward = pod::port_forward(&client, &ns, &pod_name, 80, listener, |e| {
        panic!("connection failed: {e}")
    });
    let response = http_get_through(local, forward).await;
    assert!(response.starts_with("HTTP/1.1 200"), "response: {response}");

    delete_ns(&client, &ns).await;
}

/// GET / on `local` while `forward` serves it.
async fn http_get_through(
    local: std::net::SocketAddr,
    forward: impl std::future::Future<Output = std::io::Result<()>>,
) -> String {
    let request = async {
        let mut stream = tokio::net::TcpStream::connect(local).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).into_owned()
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        tokio::select! {
            ended = forward => panic!("port-forward ended: {ended:?}"),
            response = request => response,
        }
    })
    .await
    .expect("timed out waiting for nginx through the forward")
}

/// A Service forward reaches a selected Pod at the targetPort its named
/// service port maps to.
#[tokio::test]
#[ignore = "needs a reachable cluster (kind-astrolabe-dev)"]
async fn service_port_forward_reaches_a_selected_pod() {
    let client = default_client().await.unwrap();
    let ns = unique_ns();
    running_nginx(&client, &ns).await;
    let service = format!(
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: web\n  namespace: {ns}\nspec:\n  selector:\n    app: nginx\n  ports:\n    - name: http\n      port: 8081\n      targetPort: 80\n"
    );
    let report = apply_documents(&client, &service, false).await.unwrap();
    assert!(report.ok, "service should apply: {:?}", report.outcomes);

    let listener = pod::bind_local(0).await.unwrap();
    let local = listener.local_addr().unwrap();
    let forward = service::port_forward(&client, &ns, "web", 8081, listener, |e| {
        panic!("connection failed: {e}")
    });
    let response = http_get_through(local, forward).await;
    assert!(response.starts_with("HTTP/1.1 200"), "response: {response}");

    delete_ns(&client, &ns).await;
}
