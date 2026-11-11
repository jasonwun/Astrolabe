use std::collections::{BTreeSet, HashMap};

use astrolabe_kube::cluster::ClusterId;
use astrolabe_model::builtin::BuiltinKind;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{DockArea, DockPlacement, DockSkin, PanelId, panel_handle};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::exec::ExecPanel;
use crate::launch::{LaunchOpening, LaunchTab};
use crate::logs::LogsPanel;
use crate::notice_view::{banner_view, notice_view};
use crate::open_tab::OpenTab;
use crate::resource_detail::{EditorPanel, ResourceDetailPanel};
use crate::resource_list::ResourceListPanel;
use crate::session::{ScriptedCluster, Session};
use crate::yaml_editor::YamlEditorView;
use astrolabe_model::list::crd_denied_notice;
use astrolabe_model::session::{CustomKindId, HealthIndicator};
use astrolabe_model::session_state::Viewing;
use astrolabe_model::tab::{TabKey, TabRequest};

/// A tab a request may reuse. Only a Logs or Exec tab is restarted, so
/// only those keep their panel, and weakly: the dock owns a panel, and a
/// closed tab must release its stream or shell (#341).
#[derive(Clone)]
enum OpenedTab {
    Logs(WeakEntity<LogsPanel>),
    Exec(WeakEntity<ExecPanel>),
    Other(PanelId),
}

impl OpenedTab {
    fn id(&self) -> PanelId {
        match self {
            Self::Logs(panel) => PanelId::from(panel.entity_id()),
            Self::Exec(panel) => PanelId::from(panel.entity_id()),
            Self::Other(id) => *id,
        }
    }
}

/// Key context around the session dock: `cmd-w` closes its active tab.
pub const DOCK_KEY_CONTEXT: &str = "SessionDock";

/// One cluster: resource navigation beside a DockArea whose center holds
/// the list tab plus detail and editor tabs.
pub struct SessionView {
    session: Entity<Session>,
    dock: Entity<DockArea>,
    list: Entity<ResourceListPanel>,
    /// The open tabs a request may reuse; a closed tab's entry stays until
    /// replaced (a Logs or Exec entry is weak, so it no longer keeps the
    /// panel).
    tabs: HashMap<TabKey, OpenedTab>,
    collapsed: BTreeSet<String>,
    namespace_menu: bool,
}

impl SessionView {
    pub fn new(
        cluster: ClusterId,
        namespace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = cx.new(|cx| Session::new(cluster, namespace, cx));
        Self::with_session(session, window, cx)
    }

    pub fn scripted(
        cluster: ClusterId,
        namespace: String,
        scripted: ScriptedCluster,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = cx.new(|cx| Session::scripted(cluster, namespace, scripted, cx));
        Self::with_session(session, window, cx)
    }

    fn with_session(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = cx.new(|cx| ResourceListPanel::new(session.clone(), window, cx));
        Self::forward_open_tab(&list, window, cx);
        let (dock, skin) = DockSkin::dock_area("session", None, window, cx);
        // Off by default in the skin; the panels' `closable` still gates each
        // tab.
        skin.set_close_button_visible(true, cx);
        dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                panel_handle(list.clone()),
                DockPlacement::Center,
                None,
                window,
                cx,
            )
        });
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        Self {
            session,
            dock,
            list,
            tabs: HashMap::new(),
            collapsed: BTreeSet::new(),
            namespace_menu: false,
        }
    }

    /// The Session this view shows, for views outside it that act on its
    /// Cluster: the Workspace's YAML editor (#161).
    pub fn session(&self) -> Entity<Session> {
        self.session.clone()
    }

    /// What the sidebar shows beside this Session's Cluster.
    pub fn health_indicator(&self, cx: &App) -> HealthIndicator {
        self.session.read(cx).state().health_indicator()
    }

    /// Show the launch's tabs (#106): a list in the list tab, a detail in
    /// a tab of its own, both in the session's namespace.
    pub fn open_tabs(&mut self, tabs: Vec<LaunchTab>, window: &mut Window, cx: &mut Context<Self>) {
        let namespace = self.session.read(cx).state().namespace().to_string();
        for tab in tabs {
            match tab.opening(&namespace) {
                LaunchOpening::Show(view) => self.show(view, window, cx),
                LaunchOpening::Open(request) => self.open_tab(request, window, cx),
            }
        }
    }

    fn show(&mut self, view: Viewing, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |list, cx| list.show(view, cx));
        self.select(PanelId::from(self.list.entity_id()), window, cx);
        cx.notify();
    }

    /// Display an open tab and focus it. `add_panel_view` and `select_panel`
    /// move no focus, so without this the focus stays in the tab just
    /// hidden, outside the dispatch path, and `cmd-w` reaches no tab group.
    fn select(&mut self, id: PanelId, window: &mut Window, cx: &mut Context<Self>) {
        self.dock
            .update(cx, |dock, cx| dock.select_panel(id, window, cx));
        if let Some(panel) = self.dock.read(cx).panel(id) {
            panel.focus_handle(cx).focus(window, cx);
        }
    }

    /// Open the tab a panel asked for: show the open tab with the request's
    /// key (`TabRequest::key`), else build the panel. Reopening a Logs or
    /// Exec tab whose stream has finished starts a new one in that tab.
    ///
    /// Who starts I/O: a Logs or Exec stream lives as long as its tab, so
    /// this starts it when it makes or restarts the tab. The detail panel's
    /// Scale, Delete and Forward are one-shot, so the panel calls `Session`
    /// itself.
    fn open_tab(&mut self, request: TabRequest, window: &mut Window, cx: &mut Context<Self>) {
        let key = request.key();
        if let Some(opened) = self.reusable(&key, cx) {
            // `reusable` found the panel in the dock, so a Logs or Exec
            // handle upgrades; a failed upgrade would only skip the restart.
            // Only a finished Logs or Exec tab is restarted; any other
            // reused tab is just selected. Keys are disjoint per kind, so
            // the request is of the tab's kind.
            match &opened {
                OpenedTab::Logs(panel) => {
                    if let TabRequest::Logs { request, .. } = request
                        && let Some(panel) = panel.upgrade()
                        && panel.read(cx).is_finished()
                    {
                        let stream = self.session.update(cx, |session, _| session.logs(request));
                        panel.update(cx, |panel, cx| panel.restart(stream, window, cx));
                    }
                }
                OpenedTab::Exec(panel) => {
                    if let TabRequest::Exec { request, .. } = request
                        && let Some(panel) = panel.upgrade()
                        && panel.read(cx).is_finished()
                    {
                        let session = self.session.update(cx, |session, _| session.exec(request));
                        panel.update(cx, |panel, cx| panel.restart(session, window, cx));
                    }
                }
                OpenedTab::Other(_) => {}
            }
            self.select(opened.id(), window, cx);
            return;
        }
        let opened = match request {
            TabRequest::Detail(target) => {
                let session = self.session.clone();
                let panel = cx.new(|cx| ResourceDetailPanel::new(session, target, window, cx));
                Self::forward_open_tab(&panel, window, cx);
                let id = self.add_tab(panel_handle(panel), window, cx);
                OpenedTab::Other(id)
            }
            TabRequest::Logs { title, request } => {
                let stream = self.session.update(cx, |session, _| session.logs(request));
                let panel = cx.new(|cx| LogsPanel::new(title, stream, window, cx));
                self.add_tab(panel_handle(panel.clone()), window, cx);
                OpenedTab::Logs(panel.downgrade())
            }
            TabRequest::Exec { title, request } => {
                let session = self.session.update(cx, |session, _| session.exec(request));
                let panel = cx.new(|cx| ExecPanel::new(title, session, window, cx));
                self.add_tab(panel_handle(panel.clone()), window, cx);
                OpenedTab::Exec(panel.downgrade())
            }
            TabRequest::Editor { title, yaml, .. } => {
                let session = self.session.clone();
                let editor = cx.new(|cx| YamlEditorView::with_session(window, cx, yaml, session));
                let panel = cx.new(|_| EditorPanel::new(title, editor));
                let id = self.add_tab(panel_handle(panel), window, cx);
                OpenedTab::Other(id)
            }
        };
        self.tabs.insert(key, opened);
    }

    /// The open tab for `key`, checked against the dock: a closed tab's
    /// entry stays in the map.
    fn reusable(&self, key: &TabKey, cx: &App) -> Option<OpenedTab> {
        let opened = self.tabs.get(key)?;
        self.dock
            .read(cx)
            .panel(opened.id())
            .is_some()
            .then(|| opened.clone())
    }

    /// Open the tabs a panel asks for.
    fn forward_open_tab<P: EventEmitter<OpenTab>>(
        panel: &Entity<P>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(panel, window, |this, _, event: &OpenTab, window, cx| {
            this.open_tab(event.0.clone(), window, cx);
        })
        .detach();
    }

    fn add_tab(
        &mut self,
        panel: std::sync::Arc<dyn gpui_kit::component::dock::BasePanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PanelId {
        let id = panel.panel_id(cx);
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(panel, DockPlacement::Center, None, window, cx)
        });
        self.select(id, window, cx);
        cx.notify();
        id
    }

    fn render_namespace_select(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let session = self.session.read(cx);
        let current = session.state().namespace().to_string();
        let names = session.state().namespace_names();
        let label = format!("Namespace: {current}");
        v_flex()
            .relative()
            .child(
                div()
                    .id("namespace-select")
                    .aria_label(label.clone())
                    .test_support()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .text_sm()
                    .child(format!("{label} ▾"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.namespace_menu = !this.namespace_menu;
                        cx.notify();
                    })),
            )
            .when(self.namespace_menu, |this| {
                this.child(deferred(
                    v_flex()
                        .id("namespace-menu")
                        // The menu overlaps the dock's tab bar; keep clicks
                        // off it.
                        .occlude()
                        .absolute()
                        .top(px(32.))
                        .left_0()
                        .min_w(px(200.))
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .p_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .children(names.into_iter().map(|name| {
                            let selected = name.clone();
                            div()
                                .id(SharedString::from(format!("namespace-option-{name}")))
                                .test_support()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_sm()
                                .when(name == current, |row| row.bg(theme.accent))
                                .child(name)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.namespace_menu = false;
                                    this.session.update(cx, |session, cx| {
                                        session.set_namespace(selected.clone(), cx)
                                    });
                                }))
                        })),
                ))
            })
    }

    fn render_nav(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let state = self.session.read(cx).state();
        let current = state.viewing().clone();
        let item = |id: SharedString, label: String, depth: usize, selected: bool| {
            div()
                .id(id)
                .aria_label(label.clone())
                .test_support()
                .pl(px(8. + 12. * depth as f32))
                .pr_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .text_sm()
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
                .child(label)
        };
        let heading = |label: &'static str| {
            div()
                .id(label)
                .aria_label(label)
                .test_support()
                .px_2()
                .pt_2()
                .pb_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label)
        };

        let mut nav = v_flex()
            .id("session-nav")
            .w(px(220.))
            .h_full()
            .flex_shrink_0()
            .overflow_y_scroll()
            .p_2()
            .gap_0p5()
            .border_r_1()
            .border_color(theme.sidebar_border)
            .bg(theme.sidebar)
            .child(heading("Cluster"))
            .child(
                item(
                    "nav-namespaces".into(),
                    Viewing::Namespaces.label(),
                    0,
                    current == Viewing::Namespaces,
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.show(Viewing::Namespaces, window, cx)),
                ),
            )
            .children(
                BuiltinKind::NAVIGATION
                    .into_iter()
                    .flat_map(|(section, kinds)| {
                        std::iter::once(heading(section).into_any_element()).chain(
                            kinds.iter().map(|&kind| {
                                item(
                                    kind.nav_id().into(),
                                    Viewing::Builtin(kind).label(),
                                    0,
                                    current == Viewing::Builtin(kind),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.show(Viewing::Builtin(kind), window, cx)
                                }))
                                .into_any_element()
                            }),
                        )
                    }),
            )
            // Custom Resources sits after the built-in resource sections.
            .child(heading("Custom Resources"))
            .child(
                item(
                    "nav-definitions".into(),
                    Viewing::Definitions.label(),
                    0,
                    current == Viewing::Definitions,
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.show(Viewing::Definitions, window, cx)),
                ),
            );

        if state.status().crd_access_denied {
            return nav.child(notice_view(crd_denied_notice(), cx).px_2().py_1().text_xs());
        }
        for group in state.catalog() {
            let collapsed = self.collapsed.contains(&group.group);
            let toggled = group.group.clone();
            nav = nav.child(
                item(
                    format!("nav-group-{}", group.group).into(),
                    group.group.clone(),
                    0,
                    false,
                )
                .text_color(theme.muted_foreground)
                .child(if collapsed { " ▸" } else { " ▾" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.collapsed.remove(&toggled) {
                        this.collapsed.insert(toggled.clone());
                    }
                    cx.notify();
                })),
            );
            if collapsed {
                continue;
            }
            for kind in &group.kinds {
                let id = CustomKindId::of(kind);
                let selected = current == Viewing::Custom(id.clone());
                nav = nav.child(
                    item(
                        format!("nav-kind-{}-{}", kind.group, kind.kind).into(),
                        kind.kind.clone(),
                        1,
                        selected,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show(Viewing::Custom(id.clone()), window, cx)
                    })),
                );
            }
        }
        nav
    }
}

impl Render for SessionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data = self.session.read(cx).state();
        let status = data.status();
        let banner = status.banner();
        let border = cx.theme().border;
        v_flex()
            .size_full()
            .when_some(banner, |this, notice| this.child(banner_view(notice)))
            .child(
                h_flex()
                    .p_2()
                    .border_b_1()
                    .border_color(border)
                    .child(self.render_namespace_select(cx)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_nav(cx))
                    .child(
                        div()
                            .key_context(DOCK_KEY_CONTEXT)
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.dock.clone()),
                    ),
            )
    }
}
