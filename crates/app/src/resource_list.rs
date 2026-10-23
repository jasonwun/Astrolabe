//! The list tab: one fixed center-dock panel whose content follows the
//! navigation selection (Namespaces, CRD Definitions, or a CRD kind).

use std::collections::BTreeSet;
use std::rc::Rc;

use astrolabe_kube::crd::KindResource;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::projection::{
    ResourceRow, ResourceTable, project_custom_resources, project_definitions, project_namespaces,
};
use crate::resource_detail::DetailTarget;
use crate::session::{CustomKindId, Session};
use crate::workspace::STALE_MESSAGE;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListView {
    Namespaces,
    Definitions,
    Custom(CustomKindId),
}

/// A row was clicked: open its detail tab.
pub struct OpenDetail(pub DetailTarget);

/// What the list area shows instead of (or above) the table.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ListState {
    Table,
    Message(String),
    Error(String),
}

pub struct ResourceListPanel {
    session: Entity<Session>,
    view: ListView,
    table: Entity<TableState<RowsTable>>,
    state: ListState,
    stale: bool,
    group_filter: BTreeSet<String>,
    focus_handle: FocusHandle,
}

impl ResourceListPanel {
    pub fn new(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let open_row: OpenRow = {
            let panel = cx.entity().downgrade();
            Rc::new(move |key: &str, cx: &mut App| {
                let key = key.to_string();
                panel.update(cx, |panel, cx| panel.open_row(&key, cx)).ok();
            })
        };
        let table = cx.new(|cx| {
            TableState::new(
                RowsTable {
                    table: ResourceTable::default(),
                    open_row: None,
                },
                window,
                cx,
            )
            .row_selectable(false)
            .col_selectable(false)
            .sortable(false)
            .col_movable(false)
        });
        table.update(cx, |table, _| {
            table.delegate_mut().open_row = Some(open_row)
        });
        cx.observe(&session, |panel, _, cx| panel.refresh(cx))
            .detach();
        let mut panel = Self {
            session,
            view: ListView::Namespaces,
            table,
            state: ListState::Table,
            stale: false,
            group_filter: BTreeSet::new(),
            focus_handle: cx.focus_handle(),
        };
        panel.refresh(cx);
        panel
    }

    pub fn view(&self) -> &ListView {
        &self.view
    }

    pub fn show(&mut self, view: ListView, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| match &view {
            ListView::Custom(id) => {
                session.open_custom_list(id, cx);
            }
            _ => session.close_custom_list(),
        });
        self.view = view;
        self.refresh(cx);
    }

    fn open_row(&mut self, key: &str, cx: &mut Context<Self>) {
        let (namespace, name) = match key.split_once('/') {
            Some((namespace, name)) => (Some(namespace.to_string()), name.to_string()),
            None => (None, key.to_string()),
        };
        let target = match &self.view {
            ListView::Namespaces => return,
            ListView::Definitions => DetailTarget::Definition { name },
            ListView::Custom(id) => {
                let session = self.session.read(cx);
                let Some(kind) = session.custom_kind(id) else {
                    return;
                };
                DetailTarget::Custom {
                    id: id.clone(),
                    watch: session.custom_watch_key(kind),
                    namespace,
                    name,
                }
            }
        };
        cx.emit(OpenDetail(target));
    }

    fn toggle_group(&mut self, group: String, cx: &mut Context<Self>) {
        if !self.group_filter.remove(&group) {
            self.group_filter.insert(group);
        }
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let now = Timestamp::now();
        let session = self.session.read(cx);
        let (table, state, stale) = match &self.view {
            ListView::Namespaces => (
                ResourceTable {
                    columns: vec!["Name".to_string(), "Status".to_string()],
                    rows: project_namespaces(&session.namespaces()),
                },
                ListState::Table,
                false,
            ),
            ListView::Definitions => {
                let crds = session.crds();
                let state = if session.crd_access_denied() {
                    ListState::Message(CRD_DENIED.to_string())
                } else {
                    ListState::Table
                };
                (
                    project_definitions(&crds.objects(), &self.group_filter, now),
                    state,
                    crds.show_stale_warning(),
                )
            }
            ListView::Custom(id) => match session.custom_kind(id) {
                None => (
                    ResourceTable::default(),
                    ListState::Message(format!("{} is no longer defined", id.kind)),
                    false,
                ),
                Some(kind) => match &kind.resource {
                    KindResource::Undiscovered => (
                        ResourceTable::default(),
                        ListState::Message(match session.discovery_failure() {
                            Some(error) => format!("API discovery failed: {error}"),
                            None => "Discovering API resources…".to_string(),
                        }),
                        false,
                    ),
                    KindResource::NotServed => (
                        ResourceTable::default(),
                        ListState::Error(format!(
                            "{} {} is not served by the API server",
                            kind.kind, kind.version
                        )),
                        false,
                    ),
                    KindResource::Served(_) => {
                        let watch = session.custom_watch(&session.custom_watch_key(kind));
                        let objects = watch.map(|watch| watch.objects()).unwrap_or_default();
                        let state = match watch {
                            Some(watch) if watch.forbidden() => ListState::Message(format!(
                                "No permission to list {} here",
                                kind.plural
                            )),
                            _ => ListState::Table,
                        };
                        (
                            project_custom_resources(kind, &objects, now),
                            state,
                            watch.is_some_and(|watch| watch.show_stale_warning()),
                        )
                    }
                },
            },
        };
        self.state = state;
        self.stale = stale;
        self.table.update(cx, |state, cx| {
            state.delegate_mut().table = table;
            state.refresh(cx);
        });
        cx.notify();
    }

    fn render_group_filter(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let groups: BTreeSet<String> = self
            .session
            .read(cx)
            .crds()
            .objects()
            .iter()
            .map(|crd| crd.spec.group.clone())
            .collect();
        h_flex()
            .flex_wrap()
            .gap_1()
            .p_2()
            .children(groups.into_iter().map(|group| {
                let selected = self.group_filter.contains(&group);
                let toggled = group.clone();
                div()
                    .id(SharedString::from(format!("group-filter-{group}")))
                    .aria_label(format!("Filter group {group}"))
                    .test_support()
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .text_xs()
                    .when(selected, |chip| {
                        chip.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .child(group)
                    .on_click(
                        cx.listener(move |panel, _, _, cx| panel.toggle_group(toggled.clone(), cx)),
                    )
            }))
    }
}

pub const CRD_DENIED: &str = "No permission to list CustomResourceDefinitions";

impl EventEmitter<OpenDetail> for ResourceListPanel {}
impl EventEmitter<PanelEvent> for ResourceListPanel {}

impl Focusable for ResourceListPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ResourceListPanel {
    fn panel_name(&self) -> &'static str {
        "ResourceList"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for ResourceListPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(match &self.view {
            ListView::Namespaces => "Namespaces".into(),
            ListView::Definitions => "Definitions".into(),
            ListView::Custom(id) => id.kind.clone().into(),
        })
    }

    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tab_name(cx).unwrap_or_default()
    }
}

impl Render for ResourceListPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let notice = match &self.state {
            ListState::Table => None,
            ListState::Message(text) => {
                Some(("list-message", text.clone(), theme.muted_foreground))
            }
            ListState::Error(text) => Some(("list-error", text.clone(), theme.danger)),
        };
        v_flex()
            .size_full()
            .when(self.stale, |this| {
                this.child(
                    div()
                        .id("list-stale")
                        .aria_label(STALE_MESSAGE)
                        .test_support()
                        .child(
                            gpui_kit::component::alert::Alert::warning(
                                "list-stale-alert",
                                STALE_MESSAGE,
                            )
                            .banner(),
                        ),
                )
            })
            .when(self.view == ListView::Definitions, |this| {
                this.child(self.render_group_filter(cx))
            })
            .map(|this| match notice {
                Some((id, text, color)) => this.child(
                    div()
                        .id(id)
                        .aria_label(text.clone())
                        .test_support()
                        .p_4()
                        .text_color(color)
                        .child(text),
                ),
                None => this.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(DataTable::new(&self.table).stripe(true)),
                ),
            })
    }
}

type OpenRow = Rc<dyn Fn(&str, &mut App)>;

/// Table delegate over a projected [`ResourceTable`]; the first cell of
/// each row opens its detail.
pub(crate) struct RowsTable {
    table: ResourceTable,
    open_row: Option<OpenRow>,
}

impl RowsTable {
    fn row(&self, row_ix: usize) -> &ResourceRow {
        &self.table.rows[row_ix]
    }
}

impl TableDelegate for RowsTable {
    fn columns_count(&self, _cx: &App) -> usize {
        self.table.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.table.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let name = self.table.columns[col_ix].clone();
        let width = if col_ix == 0 { 320. } else { 160. };
        Column::new(SharedString::from(format!("col-{col_ix}")), name).width(px(width))
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let name = self.table.columns[col_ix].clone();
        div()
            .id(SharedString::from(format!("column-{col_ix}")))
            .aria_label(name.clone())
            .test_support()
            .child(name)
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let row = self.row(row_ix);
        let cell = row.cells[col_ix].clone();
        if col_ix == 0
            && let Some(open_row) = self.open_row.clone()
        {
            let key = row.key.clone();
            return div()
                .id(SharedString::from(format!("row-{key}")))
                .aria_label(cell.clone())
                .test_support()
                .cursor_pointer()
                .child(cell)
                .on_click(move |_, _, cx| open_row(&key, cx))
                .into_any_element();
        }
        div()
            .id(SharedString::from(format!("cell-{row_ix}-{col_ix}")))
            .aria_label(cell.clone())
            .test_support()
            .child(cell)
            .into_any_element()
    }
}
