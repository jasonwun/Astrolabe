use std::collections::{BTreeSet, HashMap};

use astrolabe_kube::cluster::ClusterId;
use astrolabe_kube::object::ObjectKey;
use astrolabe_kube::pod::{ExecRequest, LogRequest};
use astrolabe_model::builtin::BuiltinKind;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::dock::{DockArea, DockPlacement, DockSkin, PanelId, panel_handle};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::exec::ExecPanel;
use crate::logs::LogsPanel;
use crate::resource_detail::{EditorPanel, OpenEditor, OpenExec, OpenLogs, ResourceDetailPanel};
use crate::resource_list::{CRD_DENIED, ListView, OpenDetail, ResourceListPanel};
use crate::session::{ScriptedCluster, Session};
use crate::workspace::{LaunchTab, STALE_MESSAGE};
use crate::yaml_editor::YamlEditorView;
use astrolabe_model::detail::DetailTarget;
use astrolabe_model::session::{ConnectionHealth, CustomKindId};

pub const DISCOVERY_STALE_MESSAGE: &str = "API discovery failed, resource navigation may be stale";

/// Key context around the session dock: `cmd-w` closes its active tab.
pub const DOCK_KEY_CONTEXT: &str = "SessionDock";

/// One cluster: resource navigation beside a DockArea whose center holds
/// the list tab plus detail and editor tabs.
pub struct SessionView {
    session: Entity<Session>,
    dock: Entity<DockArea>,
    list: Entity<ResourceListPanel>,
    details: HashMap<DetailTarget, Entity<ResourceDetailPanel>>,
    collapsed: BTreeSet<String>,
    namespace_menu: bool,
}

impl SessionView {
    pub fn new(
        cluster: ClusterId,
        namespace: String,
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
        cx.subscribe_in(&list, window, |this, _, event: &OpenDetail, window, cx| {
            this.open_detail(event.0.clone(), window, cx);
        })
        .detach();
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
            details: HashMap::new(),
            collapsed: BTreeSet::new(),
            namespace_menu: false,
        }
    }

    /// The Session this view shows, for views outside it that act on its
    /// Cluster: the Workspace's YAML editor (#161).
    pub fn session(&self) -> Entity<Session> {
        self.session.clone()
    }

    pub fn health(&self, cx: &App) -> ConnectionHealth {
        self.session.read(cx).state().health()
    }

    /// Show the launch's tabs (#106): a list in the list tab, a detail in
    /// a tab of its own, both in the session's namespace.
    pub fn open_tabs(&mut self, tabs: Vec<LaunchTab>, window: &mut Window, cx: &mut Context<Self>) {
        let namespace = self.session.read(cx).state().namespace().to_string();
        for tab in tabs {
            match tab {
                LaunchTab::List(kind) => self.show(ListView::Builtin(kind), window, cx),
                LaunchTab::Detail(kind, name) => {
                    let key = ObjectKey::new(Some(namespace.clone()), name);
                    self.open_detail(DetailTarget::Builtin { kind, key }, window, cx);
                }
            }
        }
    }

    fn show(&mut self, view: ListView, window: &mut Window, cx: &mut Context<Self>) {
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

    fn open_detail(&mut self, target: DetailTarget, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.details.get(&target) {
            let id = PanelId::from(panel.entity_id());
            if self.dock.read(cx).panel(id).is_some() {
                self.select(id, window, cx);
                return;
            }
        }
        let session = self.session.clone();
        let panel = cx.new(|cx| ResourceDetailPanel::new(session, target.clone(), window, cx));
        cx.subscribe_in(&panel, window, |this, _, event: &OpenEditor, window, cx| {
            this.open_editor(event.title.clone(), event.yaml.clone(), window, cx);
        })
        .detach();
        cx.subscribe_in(&panel, window, |this, _, event: &OpenDetail, window, cx| {
            this.open_detail(event.0.clone(), window, cx);
        })
        .detach();
        cx.subscribe_in(&panel, window, |this, _, event: &OpenLogs, window, cx| {
            this.open_logs(event.title.clone(), event.request.clone(), window, cx);
        })
        .detach();
        cx.subscribe_in(&panel, window, |this, _, event: &OpenExec, window, cx| {
            this.open_exec(event.title.clone(), event.request.clone(), window, cx);
        })
        .detach();
        self.add_tab(panel_handle(panel.clone()), window, cx);
        self.details.insert(target, panel);
    }

    fn open_logs(
        &mut self,
        title: String,
        request: LogRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stream = self.session.update(cx, |session, _| session.logs(request));
        let panel = cx.new(|cx| LogsPanel::new(title, stream, window, cx));
        self.add_tab(panel_handle(panel), window, cx);
    }

    fn open_exec(
        &mut self,
        title: String,
        request: ExecRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.session.update(cx, |session, _| session.exec(request));
        let panel = cx.new(|cx| ExecPanel::new(title, session, window, cx));
        self.add_tab(panel_handle(panel), window, cx);
    }

    fn open_editor(
        &mut self,
        title: String,
        yaml: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.session.clone();
        let editor = cx.new(|cx| YamlEditorView::with_session(window, cx, yaml, session));
        let panel = cx.new(|_| EditorPanel::new(title, editor));
        self.add_tab(panel_handle(panel), window, cx);
    }

    fn add_tab(
        &mut self,
        panel: std::sync::Arc<dyn gpui_kit::component::dock::BasePanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = panel.panel_id(cx);
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(panel, DockPlacement::Center, None, window, cx)
        });
        self.select(id, window, cx);
        cx.notify();
    }

    fn render_namespace_select(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let session = self.session.read(cx);
        let current = session.state().namespace().to_string();
        let names: Vec<String> = session
            .state()
            .namespaces()
            .iter()
            .filter_map(|namespace| namespace.metadata.name.clone())
            .collect();
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
        let current = self.list.read(cx).view().clone();
        let session = self.session.read(cx);
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
                    "Namespaces".to_string(),
                    0,
                    current == ListView::Namespaces,
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.show(ListView::Namespaces, window, cx)),
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
                                    kind.registration().label.to_string(),
                                    0,
                                    current == ListView::Builtin(kind),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.show(ListView::Builtin(kind), window, cx)
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
                    "Definitions".to_string(),
                    0,
                    current == ListView::Definitions,
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.show(ListView::Definitions, window, cx)),
                ),
            );

        if session.state().crd_access_denied() {
            return nav.child(
                div()
                    .id("nav-crd-denied")
                    .aria_label(CRD_DENIED)
                    .test_support()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(CRD_DENIED),
            );
        }
        for group in session.state().catalog() {
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
                let selected = current == ListView::Custom(id.clone());
                nav = nav.child(
                    item(
                        format!("nav-kind-{}-{}", kind.group, kind.kind).into(),
                        kind.kind.clone(),
                        1,
                        selected,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show(ListView::Custom(id.clone()), window, cx)
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
        let stale = data.show_stale_warning();
        // Same banner, but say what failed: the watch may be healthy.
        let message =
            if data.discovery_failure().is_some() && data.health() != ConnectionHealth::Stale {
                DISCOVERY_STALE_MESSAGE
            } else {
                STALE_MESSAGE
            };
        let border = cx.theme().border;
        v_flex()
            .size_full()
            .when(stale, |this| {
                this.child(
                    div()
                        .id("namespace-stale")
                        .role(Role::Alert)
                        .aria_label(message)
                        .test_support()
                        .child(Alert::warning("namespace-stale-alert", message).banner()),
                )
            })
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
