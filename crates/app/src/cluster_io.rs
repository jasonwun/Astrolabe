//! The session's one I/O seam: every scripted-vs-live decision lives here,
//! so `Session` starts watches, discovers, deletes and hands out its client
//! without knowing which kind of cluster it talks to. A concrete enum, not
//! a trait (workspace decision: main-thread GPUI entities, no trait
//! boundary).

use std::collections::HashMap;
use std::time::Duration;

use astrolabe_kube::discovery::ServedResources;
use astrolabe_kube::kube::Client;
use astrolabe_kube::kube::runtime::watcher::{self, Event};
use futures::future::{BoxFuture, Shared};
use futures::{FutureExt as _, SinkExt, Stream, StreamExt, channel::mpsc};
use tokio::sync::oneshot;

use crate::kube_bridge::{spawn_cluster, spawn_cluster_task};
use crate::session::{
    CrdWatchMessage, NamespaceWatchMessage, ObjectRef, ObjectWatchMessage, WatchError, WatchKey,
    WatchMessage,
};

/// Retry interval for a watch the apiserver refused with 403.
const FORBIDDEN_RETRY: Duration = Duration::from_secs(30);

type DiscoverFn = Box<dyn FnMut() -> Result<ServedResources, String>>;
type DeleteFn = Box<dyn FnMut(&ObjectRef) -> Result<(), String>>;

/// Hand-fed cluster I/O for tests and demos: watch events come from
/// channels, discovery and delete from closures.
pub struct ScriptedCluster {
    pub(crate) namespaces: Option<mpsc::Receiver<NamespaceWatchMessage>>,
    pub(crate) crds: Option<mpsc::Receiver<CrdWatchMessage>>,
    pub(crate) watches: HashMap<WatchKey, mpsc::Receiver<ObjectWatchMessage>>,
    discover: DiscoverFn,
    delete: DeleteFn,
}

impl ScriptedCluster {
    pub fn new(namespaces: mpsc::Receiver<NamespaceWatchMessage>) -> Self {
        Self {
            namespaces: Some(namespaces),
            crds: None,
            watches: HashMap::new(),
            discover: Box::new(|| Ok(ServedResources::default())),
            delete: Box::new(|_| Ok(())),
        }
    }

    pub fn crds(mut self, crds: mpsc::Receiver<CrdWatchMessage>) -> Self {
        self.crds = Some(crds);
        self
    }

    /// Events for the object watch `key` (a custom resource or built-in
    /// kind), once a view opens it.
    pub fn watch(mut self, key: WatchKey, events: mpsc::Receiver<ObjectWatchMessage>) -> Self {
        self.watches.insert(key, events);
        self
    }

    pub fn discover(
        mut self,
        discover: impl FnMut() -> Result<ServedResources, String> + 'static,
    ) -> Self {
        self.discover = Box::new(discover);
        self
    }

    pub fn delete(
        mut self,
        delete: impl FnMut(&ObjectRef) -> Result<(), String> + 'static,
    ) -> Self {
        self.delete = Box::new(delete);
        self
    }
}

/// The session's cluster client, connected once and shared by every
/// watcher and one-shot operation.
pub type SharedClient = Shared<BoxFuture<'static, Result<Client, String>>>;

pub(crate) enum ClusterIo {
    Live {
        cluster: String,
        client: SharedClient,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    },
    Scripted(ScriptedCluster),
}

impl ClusterIo {
    /// Connect to kubeconfig context `cluster`; watches and one-shot
    /// operations wait on the shared connection.
    pub(crate) fn connect(cluster: String) -> Self {
        let client = {
            let cluster = cluster.clone();
            let connect = spawn_cluster(async move {
                astrolabe_kube::client_for_context(&cluster)
                    .await
                    .map_err(|error| error.to_string())
            });
            async move {
                connect
                    .await
                    .unwrap_or_else(|_| Err("cluster connect task dropped".to_string()))
            }
            .boxed()
            .shared()
        };
        Self::Live {
            cluster,
            client,
            tasks: Vec::new(),
        }
    }

    /// Start one watch: live clusters run `stream` resident on the cluster
    /// runtime; scripted clusters hand over the channel `scripted` picks,
    /// if the test scripted one.
    pub(crate) fn watch<K, S>(
        &mut self,
        label: &'static str,
        scripted: impl FnOnce(&mut ScriptedCluster) -> Option<mpsc::Receiver<WatchMessage<K>>>,
        stream: impl Fn(&Client) -> S + Send + 'static,
    ) -> Option<mpsc::Receiver<WatchMessage<K>>>
    where
        K: Send + 'static,
        S: Stream<Item = Result<Event<K>, watcher::Error>> + Send,
    {
        match self {
            Self::Scripted(cluster) => scripted(cluster),
            Self::Live {
                cluster,
                client,
                tasks,
            } => {
                let (events, task) = spawn_watch(cluster.clone(), label, client.clone(), stream);
                tasks.push(task);
                Some(events)
            }
        }
    }

    /// API discovery; a scripted cluster answers before this returns.
    pub(crate) fn discover(&mut self) -> oneshot::Receiver<Result<ServedResources, String>> {
        match self {
            Self::Scripted(cluster) => answered((cluster.discover)()),
            Self::Live { client, .. } => {
                let client = client.clone();
                spawn_cluster(async move {
                    let client = client.await?;
                    astrolabe_kube::discovery::discover(&client)
                        .await
                        .map_err(|error| error.to_string())
                })
            }
        }
    }

    pub(crate) fn delete(&mut self, object: ObjectRef) -> oneshot::Receiver<Result<(), String>> {
        match self {
            Self::Scripted(cluster) => answered((cluster.delete)(&object)),
            Self::Live { client, .. } => {
                let client = client.clone();
                spawn_cluster(async move {
                    let client = client.await?;
                    astrolabe_kube::resource::delete(
                        &client,
                        &object.resource,
                        object.key.namespace.as_deref(),
                        &object.key.name,
                    )
                    .await
                    .map_err(|error| error.to_string())
                })
            }
        }
    }

    /// The live shared client; `None` for scripted clusters.
    pub(crate) fn client(&self) -> Option<SharedClient> {
        match self {
            Self::Live { client, .. } => Some(client.clone()),
            Self::Scripted(_) => None,
        }
    }
}

impl Drop for ClusterIo {
    fn drop(&mut self) {
        if let Self::Live { tasks, .. } = self {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

fn answered<T>(value: T) -> oneshot::Receiver<T> {
    let (tx, rx) = oneshot::channel();
    let _ = tx.send(value);
    rx
}

/// Resident watcher on the cluster runtime. Any stream error restarts the
/// watcher from a fresh LIST (#28): a silent watch resumption would emit no
/// `InitDone`, leaving the UI stale.
fn spawn_watch<K, S>(
    cluster: String,
    label: &'static str,
    client: SharedClient,
    stream: impl Fn(&Client) -> S + Send + 'static,
) -> (mpsc::Receiver<WatchMessage<K>>, tokio::task::JoinHandle<()>)
where
    K: Send + 'static,
    S: Stream<Item = Result<Event<K>, watcher::Error>> + Send,
{
    let (mut sender, receiver) = mpsc::channel(256);
    let task = spawn_cluster_task(async move {
        let client = match client.await {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(cluster = %cluster, error = %error, "{label} watch client setup failed");
                let _ = sender.send(Err(WatchError::new(error))).await;
                return;
            }
        };
        loop {
            let events = stream(&client);
            futures::pin_mut!(events);
            let mut report_end = true;
            let mut forbidden = false;
            while let Some(event) = events.next().await {
                let event = event.map_err(|error| {
                    tracing::warn!(cluster = %cluster, error = %error, "{label} watch failed");
                    WatchError::from_watcher(&error)
                });
                let failed = event.is_err();
                forbidden = matches!(&event, Err(error) if error.forbidden);
                if sender.send(event).await.is_err() {
                    return;
                }
                if failed {
                    report_end = false;
                    break;
                }
            }
            if report_end
                && sender
                    .send(Err(WatchError::new(format!("{label} watch ended"))))
                    .await
                    .is_err()
            {
                return;
            }
            tracing::info!(cluster = %cluster, "restarting {label} watch");
            // RBAC denials do not clear by retrying fast; back off further.
            let delay = if forbidden {
                FORBIDDEN_RETRY
            } else {
                Duration::from_secs(1)
            };
            tokio::time::sleep(delay).await;
        }
    });
    (receiver, task)
}
