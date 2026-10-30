use std::collections::BTreeMap;

use futures::channel::mpsc;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use crate::kube_bridge::spawn_cluster;
use crate::session::{ConnectionHealth, DEFAULT_NAMESPACE, NamespaceWatchMessage, ScriptedCluster};
use crate::session_view::SessionView;
use crate::yaml_editor::YamlEditorView;

pub const STALE_MESSAGE: &str = "connection lost, data may be stale";

/// What the launch opens instead of the first cluster in `default`; empty
/// leaves the launch as it is. Debug builds fill it from
/// `ASTROLABE_CLUSTER` and `ASTROLABE_NAMESPACE` (`main.rs`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preselection {
    /// Opened and selected at launch when it is a discovered cluster.
    pub cluster: Option<String>,
    /// The namespace the session opened at launch starts in.
    pub namespace: Option<String>,
}

pub struct WorkspaceView {
    clusters: Vec<String>,
    sources: BTreeMap<String, Option<ScriptedCluster>>,
    sessions: BTreeMap<String, Entity<SessionView>>,
    selected_cluster: Option<String>,
    yaml: Entity<YamlEditorView>,
    show_yaml: bool,
    discovery_error: Option<String>,
    /// Consumed by the launch selection; later sessions start in `default`.
    preselection: Preselection,
}

impl WorkspaceView {
    pub fn new(preselection: Preselection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let yaml = cx.new(|cx| YamlEditorView::new(window, cx, String::new()));
        let mut workspace = Self {
            clusters: Vec::new(),
            sources: BTreeMap::new(),
            sessions: BTreeMap::new(),
            selected_cluster: None,
            yaml,
            show_yaml: false,
            discovery_error: None,
            preselection,
        };
        workspace.discover_contexts(window, cx);
        workspace
    }

    pub fn from_events(
        events: Vec<(String, mpsc::Receiver<NamespaceWatchMessage>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let clusters = events
            .into_iter()
            .map(|(cluster, receiver)| (cluster, ScriptedCluster::new(receiver)))
            .collect();
        Self::from_clusters(clusters, Preselection::default(), window, cx)
    }

    pub fn from_clusters(
        scripted: Vec<(String, ScriptedCluster)>,
        preselection: Preselection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let clusters: Vec<String> = scripted
            .iter()
            .map(|(cluster, _)| cluster.clone())
            .collect();
        let sources = scripted
            .into_iter()
            .map(|(cluster, scripted)| (cluster, Some(scripted)))
            .collect();
        let yaml = cx.new(|cx| YamlEditorView::without_cluster(window, cx, String::new()));
        let mut workspace = Self {
            clusters,
            sources,
            sessions: BTreeMap::new(),
            selected_cluster: None,
            yaml,
            show_yaml: false,
            discovery_error: None,
            preselection,
        };
        workspace.select_launch_cluster(window, cx);
        workspace
    }

    fn discover_contexts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let contexts =
            spawn_cluster(async { astrolabe_kube::context_names().map_err(|e| e.to_string()) });
        cx.spawn_in(window, async move |this, cx| {
            let contexts = contexts
                .await
                .unwrap_or_else(|_| Err("cluster discovery task dropped".to_string()));
            this.update_in(cx, |workspace, window, cx| {
                match contexts {
                    Ok(clusters) => workspace.set_clusters(clusters, window, cx),
                    Err(error) => workspace.discovery_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_clusters(&mut self, clusters: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.clusters = clusters;
        for cluster in &self.clusters {
            self.sources.entry(cluster.clone()).or_insert(None);
        }
        if self.selected_cluster.is_none() {
            self.select_launch_cluster(window, cx);
        }
    }

    /// Open the preselected cluster, or the first one, in the preselected
    /// namespace. An undiscovered preselected cluster falls back to the
    /// launch without preselection: the first cluster in `default`.
    fn select_launch_cluster(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Preselection { cluster, namespace } = std::mem::take(&mut self.preselection);
        let (cluster, namespace) = match cluster {
            Some(cluster) if self.clusters.contains(&cluster) => (Some(cluster), namespace),
            Some(cluster) => {
                tracing::warn!(
                    ASTROLABE_CLUSTER = %cluster,
                    "preselected cluster is not a discovered context; opening the first one"
                );
                (self.clusters.first().cloned(), None)
            }
            None => (self.clusters.first().cloned(), namespace),
        };
        if let Some(cluster) = cluster {
            self.open_cluster(cluster, namespace, window, cx);
        }
    }

    fn select_cluster(&mut self, cluster: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_cluster(cluster, None, window, cx);
    }

    /// Select `cluster`, opening its session in `namespace` (`default` when
    /// `None`) unless it is open already.
    fn open_cluster(
        &mut self,
        cluster: String,
        namespace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.sessions.contains_key(&cluster) {
            let namespace = namespace.unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
            let session = match self
                .sources
                .get_mut(&cluster)
                .and_then(|source| source.take())
            {
                Some(scripted) => cx.new(|cx| {
                    SessionView::scripted(cluster.clone(), namespace, scripted, window, cx)
                }),
                None => cx.new(|cx| SessionView::new(cluster.clone(), namespace, window, cx)),
            };
            cx.observe(&session, |_workspace, _session, cx| cx.notify())
                .detach();
            self.sessions.insert(cluster.clone(), session);
        }
        self.selected_cluster = Some(cluster);
        self.show_yaml = false;
        cx.notify();
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut sidebar = v_flex()
            .w(px(220.))
            .h_full()
            .flex_shrink_0()
            .gap_1()
            .p_2()
            .border_r_1()
            .border_color(theme.sidebar_border)
            .bg(theme.sidebar)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Clusters"),
            );
        for cluster in self.clusters.clone() {
            let health = self
                .sessions
                .get(&cluster)
                .map(|session| session.read(cx).health(cx));
            let (health_label, health_color, tooltip) = match health {
                Some(ConnectionHealth::Healthy) => ("Healthy", theme.success, "Connected"),
                Some(ConnectionHealth::Connecting) => {
                    ("Connecting", theme.warning, "Connecting to cluster")
                }
                Some(ConnectionHealth::Stale) => ("Stale", theme.danger, STALE_MESSAGE),
                None => ("Not open", theme.muted_foreground, "Select to open session"),
            };
            let selected =
                self.selected_cluster.as_deref() == Some(cluster.as_str()) && !self.show_yaml;
            let selected_cluster = cluster.clone();
            sidebar = sidebar.child(
                h_flex()
                    .id(format!("cluster-{cluster}"))
                    .test_support()
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(if selected {
                        theme.sidebar_accent
                    } else {
                        theme.sidebar
                    })
                    .text_color(if selected {
                        theme.sidebar_accent_foreground
                    } else {
                        theme.sidebar_foreground
                    })
                    .child(
                        div()
                            .id(format!("cluster-health-{cluster}"))
                            .aria_label(format!("{cluster}: {health_label}"))
                            .test_support()
                            .w_2()
                            .h_2()
                            .rounded_full()
                            .bg(health_color)
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(tooltip)
                                    .build(window, cx)
                            }),
                    )
                    .child(cluster)
                    .on_click(cx.listener(move |workspace, _, window, cx| {
                        workspace.select_cluster(selected_cluster.clone(), window, cx);
                    })),
            );
        }
        sidebar.child(
            div()
                .id("yaml-editor")
                .test_support()
                .px_2()
                .py_1()
                .mt_2()
                .rounded_md()
                .cursor_pointer()
                .bg(if self.show_yaml {
                    theme.sidebar_accent
                } else {
                    theme.sidebar
                })
                .text_color(if self.show_yaml {
                    theme.sidebar_accent_foreground
                } else {
                    theme.sidebar_foreground
                })
                .child("YAML editor")
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.show_yaml = true;
                    cx.notify();
                })),
        )
    }

    fn render_content(&self) -> AnyElement {
        if self.show_yaml {
            return div()
                .id("yaml-workspace")
                .test_support()
                .size_full()
                .child(self.yaml.clone())
                .into_any_element();
        }
        if let Some(session) = self
            .selected_cluster
            .as_ref()
            .and_then(|cluster| self.sessions.get(cluster))
        {
            return div().size_full().child(session.clone()).into_any_element();
        }
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .child(
                self.discovery_error
                    .clone()
                    .unwrap_or_else(|| "Loading clusters…".to_string()),
            )
            .into_any_element()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().size_full().child(self.render_sidebar(cx)).child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(self.render_content()),
        )
    }
}
