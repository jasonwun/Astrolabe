//! The list tab: one fixed center-dock panel whose content follows the
//! navigation selection (Namespaces, a built-in kind, CRD Definitions, or a
//! CRD kind).

use std::collections::BTreeSet;
use std::rc::Rc;

use astrolabe_kube::crd::KindResource;
use astrolabe_kube::k8s_openapi::api::core::v1::ObjectReference;
use astrolabe_kube::kube::core::DynamicObject;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::builtin::BuiltinKind;
use crate::clock;
use crate::projection::{
    ResourceRow, ResourceTable, column_widths, default_column_weights, project_custom_resources,
    project_definitions, project_namespaces, track_ages,
};
use crate::resource_detail::DetailTarget;
use crate::session::{CustomKindId, Session};
use crate::store::ObjectKey;
use crate::workspace::STALE_MESSAGE;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListView {
    Namespaces,
    Builtin(BuiltinKind),
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
    notice: Option<String>,
    /// Re-projects when the youngest shown age next changes (#70);
    /// replaced on every refresh, and dropped with the panel.
    tick: Task<()>,
    focus_handle: FocusHandle,
}

impl ResourceListPanel {
    pub fn new(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let open_row: OpenRow = {
            let panel = cx.entity().downgrade();
            Rc::new(move |key: &ObjectKey, cx: &mut App| {
                panel.update(cx, |panel, cx| panel.open_row(key, cx)).ok();
            })
        };
        let table = cx.new(|cx| {
            TableState::new(
                RowsTable {
                    table: ResourceTable::default(),
                    weights: Vec::new(),
                    measured_width: 0.,
                    widths: Vec::new(),
                    open_row: None,
                    open_all_cells: false,
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
            notice: None,
            tick: Task::ready(()),
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
            ListView::Builtin(kind) => {
                session.open_builtin_list(*kind, cx);
            }
            ListView::Custom(id) => {
                session.open_custom_list(id, cx);
            }
            ListView::Namespaces | ListView::Definitions => session.close_list(),
        });
        self.view = view;
        self.notice = None;
        self.refresh(cx);
    }

    fn open_row(&mut self, key: &ObjectKey, cx: &mut Context<Self>) {
        let target = match &self.view {
            ListView::Namespaces => Ok(DetailTarget::Namespace {
                name: key.name.clone(),
            }),
            ListView::Builtin(kind) => match kind.registration().row_target {
                Some(row_target) => self.referenced_target(*kind, key, row_target, cx),
                None => Ok(DetailTarget::Builtin {
                    kind: *kind,
                    key: key.clone(),
                }),
            },
            ListView::Definitions => Ok(DetailTarget::Definition {
                name: key.name.clone(),
            }),
            ListView::Custom(id) => Ok(DetailTarget::Custom {
                id: id.clone(),
                key: key.clone(),
            }),
        };
        match target {
            Ok(target) => {
                self.notice = None;
                cx.emit(OpenDetail(target));
            }
            Err(notice) => {
                self.notice = Some(notice);
                cx.notify();
            }
        }
    }

    /// The object a row of `kind` names through its registration's
    /// `row_target`, as a detail target.
    fn referenced_target(
        &self,
        kind: BuiltinKind,
        key: &ObjectKey,
        row_target: fn(&DynamicObject) -> Option<ObjectReference>,
        cx: &App,
    ) -> Result<DetailTarget, String> {
        let session = self.session.read(cx);
        let namespace = key.namespace.as_deref().unwrap_or_default();
        let (object, reference) = session
            .object_watch(&session.builtin_watch_key(kind, namespace))
            .and_then(|watch| watch.get(Some(namespace), &key.name))
            .and_then(|object| Some((object, row_target(object)?)))
            .ok_or_else(|| format!("Cannot open {} {key}: object is unavailable", kind.kind()))?;
        DetailTarget::by_reference(reference, object.metadata.namespace.clone(), session)
    }

    fn toggle_group(&mut self, group: String, cx: &mut Context<Self>) {
        if !self.group_filter.remove(&group) {
            self.group_filter.insert(group);
        }
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let now = clock::now(cx);
        let session = self.session.read(cx);
        let weights = match &self.view {
            ListView::Builtin(kind) => kind.registration().column_weights,
            _ => None,
        };
        let ((table, state, stale), next_tick) = track_ages(now, || match &self.view {
            ListView::Namespaces => (
                ResourceTable {
                    columns: vec!["Name".to_string(), "Status".to_string()],
                    rows: project_namespaces(&session.namespaces()),
                },
                ListState::Table,
                false,
            ),
            ListView::Builtin(kind) => {
                let registration = kind.registration();
                let watch =
                    session.object_watch(&session.builtin_watch_key(*kind, session.namespace()));
                let objects = watch.map(|watch| watch.objects()).unwrap_or_default();
                let state = match watch {
                    Some(watch) if watch.forbidden() => ListState::Message(format!(
                        "No permission to list {} here",
                        registration.label.to_lowercase()
                    )),
                    _ => ListState::Table,
                };
                (
                    (registration.list)(&objects, now),
                    state,
                    watch.is_some_and(|watch| watch.show_stale_warning()),
                )
            }
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
                        let watch = session.object_watch(&session.custom_watch_key(kind));
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
        });
        self.state = state;
        self.stale = stale;
        self.tick = cx.spawn(async move |panel, cx| {
            cx.background_executor().timer(next_tick).await;
            panel.update(cx, |panel, cx| panel.refresh(cx)).ok();
        });
        let open_all_cells = match &self.view {
            ListView::Builtin(kind) => kind.registration().open_all_cells,
            _ => false,
        };
        self.table.update(cx, |state, cx| {
            debug_assert!(
                weights.is_none_or(|weights| weights.len() == table.columns.len()),
                "column weights do not match the list's columns"
            );
            let weights = weights
                .filter(|weights| weights.len() == table.columns.len())
                .map_or_else(
                    || default_column_weights(table.columns.len()),
                    <[f32]>::to_vec,
                );
            let rows = state.delegate_mut();
            rows.table = table;
            rows.weights = weights;
            rows.open_all_cells = open_all_cells;
            rows.fit_columns();
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
            ListView::Builtin(kind) => kind.registration().label.into(),
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
            // The dock focuses the displayed panel; `cmd-w` dispatches from here.
            .track_focus(&self.focus_handle)
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
            .when_some(self.notice.clone(), |this, text| {
                this.child(
                    div()
                        .id("list-notice")
                        .aria_label(text.clone())
                        .test_support()
                        .p_2()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(text),
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
                        .id("list-table")
                        .flex_1()
                        .min_h_0()
                        .child(DataTable::new(&self.table).stripe(true))
                        // Refit the columns whenever the panel's width changes.
                        .on_prepaint({
                            let table = self.table.clone();
                            move |bounds, _, cx| {
                                table.update(cx, |state, cx| {
                                    // The table's 1px border on each side.
                                    let inner = f32::from(bounds.size.width) - 2.;
                                    if state.delegate_mut().set_width(inner) {
                                        state.refresh(cx);
                                        cx.notify();
                                    }
                                })
                            }
                        })
                        .test_support(),
                ),
            })
    }
}

type OpenRow = Rc<dyn Fn(&ObjectKey, &mut App)>;

/// Table delegate over a projected [`ResourceTable`]; the first cell opens
/// details, or every cell when the kind's registration says so. The
/// columns share the table's width by weight.
pub(crate) struct RowsTable {
    table: ResourceTable,
    weights: Vec<f32>,
    /// The table's last measured inner width.
    measured_width: f32,
    /// `weights` fitted to `measured_width`.
    widths: Vec<f32>,
    open_row: Option<OpenRow>,
    open_all_cells: bool,
}

impl RowsTable {
    fn row(&self, row_ix: usize) -> &ResourceRow {
        &self.table.rows[row_ix]
    }

    fn fit_columns(&mut self) {
        self.widths = column_widths(self.measured_width, &self.weights);
    }

    /// Refits the columns to a newly measured width; false when it is
    /// unchanged.
    fn set_width(&mut self, width: f32) -> bool {
        if self.measured_width == width {
            return false;
        }
        self.measured_width = width;
        self.fit_columns();
        true
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
        Column::new(SharedString::from(format!("col-{col_ix}")), name)
            .width(px(self.widths[col_ix]))
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // The table already labels the enclosing `col-header` with the
        // column name, so the header text carries no label of its own.
        let name = self.table.columns[col_ix].clone();
        div()
            .id(SharedString::from(format!("column-{col_ix}")))
            .w_full()
            .text_ellipsis()
            .child(name)
            .test_support()
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
        if (col_ix == 0 || self.open_all_cells)
            && let Some(open_row) = self.open_row.clone()
        {
            let key = row.key.clone();
            let id = if col_ix == 0 {
                format!("row-{key}")
            } else {
                format!("event-row-{key}-{col_ix}")
            };
            return text_cell(id, cell)
                .test_support()
                .cursor_pointer()
                .on_click(move |_, _, cx| open_row(&key, cx))
                .into_any_element();
        }
        text_cell(format!("cell-{row_ix}-{col_ix}"), cell)
            .test_support()
            .into_any_element()
    }
}

/// A labeled body cell whose overlong text ends in an ellipsis.
fn text_cell(id: String, text: String) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .aria_label(text.clone())
        .w_full()
        .text_ellipsis()
        .child(text)
}
