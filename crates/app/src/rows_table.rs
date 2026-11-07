//! The rows table shared by the list tab and a detail tab's Related section.

use std::rc::Rc;

use astrolabe_kube::object::ObjectKey;
use astrolabe_model::projection::{ResourceRow, ResourceTable, column_widths};
use gpui_kit::base::ElementExt as _;
use gpui_kit::base::test_support::Observed;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

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
    pub(crate) open_all_cells: bool,
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
    pub(crate) fn list(open_row: OpenRow) -> Self {
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
