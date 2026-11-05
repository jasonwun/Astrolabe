//! The list tab: one fixed center-dock panel whose content follows the
//! navigation selection (Namespaces, a built-in kind, CRD Definitions, or a
//! CRD kind).

use std::collections::BTreeSet;
use std::rc::Rc;

use astrolabe_kube::object::ObjectKey;
use astrolabe_model::builtin::{BuiltinKind, Registration};
use astrolabe_model::projection::{ResourceRow, ResourceTable, column_widths, track_ages};
use gpui_kit::base::ElementExt as _;
use gpui_kit::base::test_support::Observed;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::clock;
use crate::perf;
use crate::session::Session;
use crate::workspace::STALE_MESSAGE;
use astrolabe_model::detail::{DetailTarget, ReferenceError};
use astrolabe_model::list::{self, ListOutcome};
use astrolabe_model::session::CustomKindId;

/// Why a row cannot be opened; shown as the list's notice.
#[derive(Debug, PartialEq, Eq)]
enum OpenRowError {
    /// The row's object left its watch, or its kind has no reference to follow.
    ObjectUnavailable {
        kind: String,
        key: ObjectKey,
    },
    Reference(ReferenceError),
}

impl std::fmt::Display for OpenRowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ObjectUnavailable { kind, key } => {
                write!(f, "Cannot open {kind} {key}: object is unavailable")
            }
            Self::Reference(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for OpenRowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ObjectUnavailable { .. } => None,
            Self::Reference(error) => Some(error),
        }
    }
}

impl From<ReferenceError> for OpenRowError {
    fn from(error: ReferenceError) -> Self {
        Self::Reference(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListView {
    Namespaces,
    Builtin(BuiltinKind),
    Definitions,
    Custom(CustomKindId),
}

impl ListView {
    /// The registration behind a built-in kind's list.
    fn registration(&self) -> Option<&'static Registration> {
        match self {
            ListView::Builtin(kind) => Some(kind.registration()),
            _ => None,
        }
    }
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
        let open_row = open_row_with(cx, Self::open_row);
        let table = cx.new(|cx| rows_table_state(RowsTable::list(open_row), window, cx));
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
            ListView::Namespaces => session.open_namespaces_list(),
            ListView::Definitions => session.open_definitions_list(),
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
                Some(_) => self.referenced_target(*kind, key, cx),
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
                self.notice = Some(notice.to_string());
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
        cx: &App,
    ) -> Result<DetailTarget, OpenRowError> {
        let row_target = kind.registration().row_target;
        let session = self.session.read(cx);
        let (object, reference) = session
            .state()
            .builtin_object(kind, key.namespace.as_deref(), &key.name)
            .and_then(|object| Some((object, row_target?(object)?)))
            .ok_or_else(|| OpenRowError::ObjectUnavailable {
                kind: kind.kind(),
                key: key.clone(),
            })?;
        DetailTarget::by_reference(
            reference,
            object.metadata.namespace.clone(),
            session.state(),
        )
        .map_err(OpenRowError::from)
    }

    fn toggle_group(&mut self, group: String, cx: &mut Context<Self>) {
        if !self.group_filter.remove(&group) {
            self.group_filter.insert(group);
        }
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let _cost = perf::time(perf::Work::ListRefresh);
        let now = clock::now(cx);
        let data = self.session.read(cx).state();
        let (resolved, next_tick) =
            track_ages(now, || list::resolve(data, &self.group_filter, now));
        let (table, state) = match resolved.outcome {
            ListOutcome::Rows(table) => (table, ListState::Table),
            ListOutcome::Loading(text)
            | ListOutcome::Forbidden(text)
            | ListOutcome::Undefined(text) => (ResourceTable::default(), ListState::Message(text)),
            ListOutcome::NotServed(text) => (ResourceTable::default(), ListState::Error(text)),
        };
        let stale = resolved.stale;
        self.state = state;
        self.stale = stale;
        self.tick = cx.spawn(async move |panel, cx| {
            cx.background_executor().timer(next_tick).await;
            panel.update(cx, |panel, cx| panel.refresh(cx)).ok();
        });
        let open_all_cells = self
            .view
            .registration()
            .is_some_and(|registration| registration.open_all_cells);
        self.table.update(cx, |state, cx| {
            let rows = state.delegate_mut();
            rows.open_all_cells = open_all_cells;
            rows.set_table(table);
            state.refresh(cx);
        });
        cx.notify();
    }

    fn render_group_filter(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let groups: BTreeSet<String> = self
            .session
            .read(cx)
            .state()
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
            // The dock focuses the displayed panel; `cmd-w` dispatches from
            // here.
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
                None => this.child(fitted_table(&self.table, "list-table").flex_1().min_h_0()),
            })
    }
}

/// Opens the object a row names.
pub(crate) type OpenRow = Rc<dyn Fn(&ObjectKey, &mut App)>;

/// An [`OpenRow`] that calls `open` on the panel `cx` belongs to, while that
/// panel lives.
pub(crate) fn open_row_with<P: 'static>(
    cx: &mut Context<P>,
    open: fn(&mut P, &ObjectKey, &mut Context<P>),
) -> OpenRow {
    let panel = cx.entity().downgrade();
    Rc::new(move |key: &ObjectKey, cx: &mut App| {
        panel.update(cx, |panel, cx| open(panel, key, cx)).ok();
    })
}

/// A [`TableState`] over `rows`, with selection, sorting and column moves
/// off.
pub(crate) fn rows_table_state(
    rows: RowsTable,
    window: &mut Window,
    cx: &mut Context<TableState<RowsTable>>,
) -> TableState<RowsTable> {
    TableState::new(rows, window, cx)
        .row_selectable(false)
        .col_selectable(false)
        .sortable(false)
        .col_movable(false)
}

/// `table` drawn in an element `id` that refits its columns whenever its
/// width changes. The caller sizes the element; the table fills it.
pub(crate) fn fitted_table(
    table: &Entity<TableState<RowsTable>>,
    id: &'static str,
) -> Observed<Stateful<Div>> {
    div()
        .id(id)
        .child(DataTable::new(table).stripe(true))
        .on_prepaint({
            let table = table.clone();
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
        .test_support()
}

/// Table delegate over a projected [`ResourceTable`]; the first cell opens
/// details, or every cell when the kind's registration says so. The
/// columns share the table's width by weight.
pub(crate) struct RowsTable {
    table: ResourceTable,
    /// The table's last measured inner width.
    measured_width: f32,
    /// The table's column weights fitted to `measured_width`.
    widths: Vec<f32>,
    open_row: OpenRow,
    open_all_cells: bool,
    place: TablePlace,
}

/// Where a [`RowsTable`] is drawn, which sets its ids and its link style.
#[derive(Clone, Copy)]
enum TablePlace {
    /// The list tab: plain ids, plain text.
    List,
    /// A detail tab's Related section: `related-` ids, so they differ from
    /// the list's, and the cells that open a row drawn as links.
    Related,
}

impl TablePlace {
    /// Prefixes every row, cell and header id.
    fn id_prefix(self) -> &'static str {
        match self {
            Self::List => "",
            Self::Related => "related-",
        }
    }

    /// Draws the cells that open a row as links.
    fn links_rows(self) -> bool {
        matches!(self, Self::Related)
    }
}

impl RowsTable {
    /// The list tab's table.
    fn list(open_row: OpenRow) -> Self {
        Self::new(open_row, TablePlace::List)
    }

    /// A detail tab's Related table.
    pub(crate) fn related(open_row: OpenRow) -> Self {
        Self::new(open_row, TablePlace::Related)
    }

    fn new(open_row: OpenRow, place: TablePlace) -> Self {
        Self {
            table: ResourceTable::default(),
            measured_width: 0.,
            widths: Vec::new(),
            open_row,
            open_all_cells: false,
            place,
        }
    }

    fn row(&self, row_ix: usize) -> &ResourceRow {
        &self.table.rows[row_ix]
    }

    /// The table shown.
    pub(crate) fn table(&self) -> &ResourceTable {
        &self.table
    }

    /// Shows `table`, its columns fitted to the last measured width. The
    /// caller refreshes the [`TableState`].
    pub(crate) fn set_table(&mut self, table: ResourceTable) {
        self.table = table;
        self.fit_columns();
    }

    fn fit_columns(&mut self) {
        self.widths = column_widths(self.measured_width, &self.table.weights());
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
        let name = self.table.columns[col_ix].header.clone();
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
        let name = self.table.columns[col_ix].header.clone();
        div()
            .id(SharedString::from(format!(
                "{}column-{col_ix}",
                self.place.id_prefix()
            )))
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
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let prefix = self.place.id_prefix();
        let link = cx.theme().link;
        let row = self.row(row_ix);
        let cell = row.cells[col_ix].clone();
        if col_ix == 0 || self.open_all_cells {
            let open_row = self.open_row.clone();
            let key = row.key.clone();
            let id = if col_ix == 0 {
                format!("{prefix}row-{key}")
            } else {
                format!("{prefix}event-row-{key}-{col_ix}")
            };
            return text_cell(id, cell)
                .when(self.place.links_rows(), |cell| cell.text_color(link))
                .test_support()
                .cursor_pointer()
                .on_click(move |_, _, cx| open_row(&key, cx))
                .into_any_element();
        }
        text_cell(format!("{prefix}cell-{row_ix}-{col_ix}"), cell)
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

#[cfg(test)]
mod open_row_error_tests {
    use super::{ObjectKey, OpenRowError, ReferenceError};

    #[test]
    fn an_unavailable_object_names_its_kind_and_key() {
        let error = OpenRowError::ObjectUnavailable {
            kind: "Event".to_string(),
            key: ObjectKey::new(Some("default".to_string()), "e1"),
        };
        assert_eq!(
            error.to_string(),
            "Cannot open Event default/e1: object is unavailable"
        );
    }

    #[test]
    fn a_reference_error_shows_its_own_text() {
        let reference = ReferenceError::MissingName {
            kind: "Pod".to_string(),
        };
        let error = OpenRowError::from(reference.clone());
        assert_eq!(error, OpenRowError::Reference(reference.clone()));
        assert_eq!(error.to_string(), reference.to_string());
    }
}
