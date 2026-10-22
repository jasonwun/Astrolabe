use std::collections::BTreeMap;

use futures::channel::mpsc;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use crate::kube_bridge::spawn_cluster;
use crate::session::{ConnectionHealth, NamespaceWatchMessage};
use crate::session_view::SessionView;
use crate::yaml_editor::YamlEditorView;

pub const STALE_MESSAGE: &str = "connection lost, data may be stale";

pub struct WorkspaceView {
    clusters: Vec<String>,
    sources: BTreeMap<String, Option<mpsc::Receiver<NamespaceWatchMessage>>>,
    sessions: BTreeMap<String, Entity<SessionView>>,
    selected_cluster: Option<String>,
    yaml: Entity<YamlEditorView>,
    show_yaml: bool,
    discovery_error: Option<String>,
}

impl WorkspaceView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let yaml = cx.new(|cx| YamlEditorView::new(window, cx, String::new()));
        let mut workspace = Self {
            clusters: Vec::new(),
            sources: BTreeMap::new(),
            sessions: BTreeMap::new(),
            selected_cluster: None,
            yaml,
            show_yaml: false,
            discovery_error: None,
        };
        workspace.discover_contexts(window, cx);
        workspace
    }

    pub fn from_events(
        events: Vec<(String, mpsc::Receiver<NamespaceWatchMessage>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let clusters: Vec<String> = events.iter().map(|(cluster, _)| cluster.clone()).collect();
        let sources = events
            .into_iter()
            .map(|(cluster, receiver)| (cluster, Some(receiver)))
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
        };
        if let Some(cluster) = workspace.clusters.first().cloned() {
            workspace.select_cluster(cluster, window, cx);
        }
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
        if self.selected_cluster.is_none()
            && let Some(cluster) = self.clusters.first().cloned()
        {
            self.select_cluster(cluster, window, cx);
        }
    }

    fn select_cluster(&mut self, cluster: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sessions.contains_key(&cluster) {
            let session = match self
                .sources
                .get_mut(&cluster)
                .and_then(|source| source.take())
            {
                Some(receiver) => {
                    cx.new(|cx| SessionView::from_events(cluster.clone(), receiver, window, cx))
                }
                None => cx.new(|cx| SessionView::new(cluster.clone(), window, cx)),
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
