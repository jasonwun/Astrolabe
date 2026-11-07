use std::collections::BTreeMap;

use futures::channel::mpsc;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use crate::launch::Preselection;
use crate::notice_view::intent_color;
use crate::perf;
use crate::session::{DEFAULT_NAMESPACE, ScriptedCluster};
use crate::session_view::SessionView;
use crate::yaml_editor::YamlEditorView;
use astrolabe_kube::cluster::{ClusterError, ClusterId};
use astrolabe_kube::watch::NamespaceWatchMessage;

pub struct WorkspaceView {
    clusters: Vec<ClusterId>,
    sources: BTreeMap<ClusterId, Option<ScriptedCluster>>,
    sessions: BTreeMap<ClusterId, Entity<SessionView>>,
    selected_cluster: Option<ClusterId>,
    /// Applies to the selected Cluster's Session, and nowhere while none
    /// is selected (#161).
    yaml: Entity<YamlEditorView>,
    show_yaml: bool,
    /// Why the Clusters could not be listed.
    listing_error: Option<ClusterError>,
    /// Consumed by the launch selection; later sessions start in `default`.
    preselection: Preselection,
}

impl WorkspaceView {
    pub fn new(preselection: Preselection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut workspace = Self::empty(Vec::new(), BTreeMap::new(), preselection, window, cx);
        workspace.list_clusters(window, cx);
        workspace
    }

    /// A workspace with no session open yet.
    fn empty(
        clusters: Vec<ClusterId>,
        sources: BTreeMap<ClusterId, Option<ScriptedCluster>>,
        preselection: Preselection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            clusters,
            sources,
            sessions: BTreeMap::new(),
            selected_cluster: None,
            yaml: cx.new(|cx| YamlEditorView::without_session(window, cx, String::new())),
            show_yaml: false,
            listing_error: None,
            preselection,
        }
    }

    pub fn from_events(
        events: Vec<(ClusterId, mpsc::Receiver<NamespaceWatchMessage>)>,
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
        scripted: Vec<(ClusterId, ScriptedCluster)>,
        preselection: Preselection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let clusters: Vec<ClusterId> = scripted
            .iter()
            .map(|(cluster, _)| cluster.clone())
            .collect();
        let sources = scripted
            .into_iter()
            .map(|(cluster, scripted)| (cluster, Some(scripted)))
            .collect();
        let mut workspace = Self::empty(clusters, sources, preselection, window, cx);
        workspace.select_launch_cluster(window, cx);
        workspace
    }

    fn list_clusters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Listing reads the kubeconfig, blocking file I/O: off the main thread.
        let clusters = cx.background_spawn(async { astrolabe_kube::clusters() });
        cx.spawn_in(window, async move |this, cx| {
            let clusters = clusters.await;
            this.update_in(cx, |workspace, window, cx| {
                match clusters {
                    Ok(clusters) => workspace.set_clusters(clusters, window, cx),
                    Err(error) => workspace.listing_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_clusters(
        &mut self,
        clusters: Vec<ClusterId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clusters = clusters;
        for cluster in &self.clusters {
            self.sources.entry(cluster.clone()).or_insert(None);
        }
        if self.selected_cluster.is_none() {
            self.select_launch_cluster(window, cx);
        }
    }

    /// Open the preselected cluster, or the first one, in the preselected
    /// namespace. An unlisted preselected cluster falls back to the
    /// launch without preselection: the first cluster in its context's
    /// namespace.
    fn select_launch_cluster(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Preselection {
            cluster,
            namespace,
            open,
        } = std::mem::take(&mut self.preselection);
        let (cluster, namespace) = match cluster {
            Some(name) => match self.clusters.iter().find(|cluster| cluster.name() == name) {
                Some(cluster) => (Some(cluster.clone()), namespace),
                None => {
                    tracing::warn!(
                        ASTROLABE_CLUSTER = %name,
                        "preselected cluster is not a listed cluster; opening the first one"
                    );
                    (self.clusters.first().cloned(), None)
                }
            },
            None => (self.clusters.first().cloned(), namespace),
        };
        if let Some(cluster) = cluster {
            self.open_cluster(cluster.clone(), namespace, window, cx);
            if let Some(session) = self.sessions.get(&cluster) {
                session.update(cx, |session, cx| session.open_tabs(open, window, cx));
            }
        }
    }

    fn select_cluster(&mut self, cluster: ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        self.open_cluster(cluster, None, window, cx);
    }

    /// Select `cluster`, opening its session in `namespace` (the context's
    /// own when `None`) unless it is open already.
    fn open_cluster(
        &mut self,
        cluster: ClusterId,
        namespace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.sessions.contains_key(&cluster) {
            let session = match self
                .sources
                .get_mut(&cluster)
                .and_then(|source| source.take())
            {
                Some(scripted) => {
                    let namespace = namespace.unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
                    cx.new(|cx| {
                        SessionView::scripted(cluster.clone(), namespace, scripted, window, cx)
                    })
                }
                None => cx.new(|cx| SessionView::new(cluster.clone(), namespace, window, cx)),
            };
            cx.observe(&session, |_workspace, _session, cx| cx.notify())
                .detach();
            self.sessions.insert(cluster.clone(), session);
        }
        let session = self.sessions[&cluster].read(cx).session();
        self.yaml
            .update(cx, |yaml, cx| yaml.set_session(session, cx));
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
                Some(health) => {
                    let indicator = health.indicator();
                    (
                        indicator.label,
                        intent_color(indicator.intent, theme),
                        indicator.tooltip,
                    )
                }
                None => ("Not open", theme.muted_foreground, "Select to open session"),
            };
            let selected = self.selected_cluster.as_ref() == Some(&cluster) && !self.show_yaml;
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
                    .child(cluster.to_string())
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
                .on_click(cx.listener(|workspace, _, window, cx| {
                    workspace.show_yaml = true;
                    workspace.yaml.focus_handle(cx).focus(window, cx);
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
                self.listing_error
                    .as_ref()
                    .map_or_else(|| "Loading clusters…".to_string(), ToString::to_string),
            )
            .into_any_element()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        perf::count(perf::Event::Draw);
        h_flex().size_full().child(self.render_sidebar(cx)).child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(self.render_content()),
        )
    }
}
