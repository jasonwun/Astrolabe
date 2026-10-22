use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::{SinkExt, StreamExt, channel::mpsc};
use gpui_kit::Context;

use crate::kube_bridge::spawn_cluster_task;
use crate::store::Store;

pub type NamespaceWatchMessage = Result<Event<Namespace>, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Namespaces,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchKey {
    pub cluster: String,
    pub kind: ResourceKind,
    pub namespace: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionHealth {
    Connecting,
    Healthy,
    Stale,
}

pub struct Session {
    watch_key: WatchKey,
    namespaces: Store<Namespace>,
    health: ConnectionHealth,
    has_snapshot: bool,
    stale: bool,
    watcher_task: Option<tokio::task::JoinHandle<()>>,
}

impl Session {
    pub fn new(cluster: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let cluster = cluster.into();
        let watch_cluster = cluster.clone();
        let (mut sender, receiver) = mpsc::channel(256);
        let watcher_task = spawn_cluster_task(async move {
            let client = match astrolabe_kube::client_for_context(&watch_cluster).await {
                Ok(client) => client,
                Err(error) => {
                    let _ = sender.send(Err(error.to_string())).await;
                    return;
                }
            };
            loop {
                let events = astrolabe_kube::watch::namespaces(&client);
                futures::pin_mut!(events);
                let mut report_end = true;
                while let Some(event) = events.next().await {
                    let failed = event.is_err();
                    if sender
                        .send(event.map_err(|error| error.to_string()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if failed {
                        report_end = false;
                        break;
                    }
                }
                if report_end
                    && sender
                        .send(Err("namespace watch ended".to_string()))
                        .await
                        .is_err()
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
        Self::with_watcher(cluster, receiver, Some(watcher_task), cx)
    }

    pub fn from_events(
        cluster: impl Into<String>,
        receiver: mpsc::Receiver<NamespaceWatchMessage>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_watcher(cluster.into(), receiver, None, cx)
    }

    fn with_watcher(
        cluster: String,
        receiver: mpsc::Receiver<NamespaceWatchMessage>,
        watcher_task: Option<tokio::task::JoinHandle<()>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = Self {
            watch_key: WatchKey {
                cluster,
                kind: ResourceKind::Namespaces,
                namespace: None,
            },
            namespaces: Store::new(),
            health: ConnectionHealth::Connecting,
            has_snapshot: false,
            stale: false,
            watcher_task,
        };
        session.spawn_drain(receiver, cx);
        session
    }

    fn spawn_drain(
        &self,
        mut receiver: mpsc::Receiver<NamespaceWatchMessage>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some(message) = receiver.next().await {
                let mut closed = false;
                if this
                    .update(cx, |session, cx| {
                        session.apply(message);
                        loop {
                            match receiver.try_recv() {
                                Ok(message) => session.apply(message),
                                Err(error) if error.is_closed() => {
                                    closed = true;
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                        if closed {
                            session.health = ConnectionHealth::Stale;
                            session.stale = session.has_snapshot;
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                if closed {
                    return;
                }
            }
            this.update(cx, |session, cx| {
                session.health = ConnectionHealth::Stale;
                session.stale = session.has_snapshot;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply(&mut self, message: NamespaceWatchMessage) {
        match message {
            Ok(event) => {
                match &event {
                    Event::Init => {
                        self.health = ConnectionHealth::Connecting;
                        self.stale = self.has_snapshot;
                    }
                    Event::InitApply(_) => {}
                    Event::InitDone => {
                        self.has_snapshot = true;
                        self.health = ConnectionHealth::Healthy;
                        self.stale = false;
                    }
                    Event::Apply(_) | Event::Delete(_) => {
                        self.has_snapshot = true;
                        self.health = ConnectionHealth::Healthy;
                        self.stale = false;
                    }
                }
                self.namespaces.apply(event);
            }
            Err(_) => {
                self.health = ConnectionHealth::Stale;
                self.stale = self.has_snapshot;
            }
        }
    }

    pub fn watch_key(&self) -> &WatchKey {
        &self.watch_key
    }

    pub fn namespaces(&self) -> Vec<&Namespace> {
        self.namespaces.snapshot()
    }

    pub fn health(&self) -> ConnectionHealth {
        self.health
    }

    pub fn show_stale_warning(&self) -> bool {
        self.stale
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(task) = self.watcher_task.take() {
            task.abort();
        }
    }
}
