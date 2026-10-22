use futures::channel::mpsc;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::v_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::projection::{ResourceRow, project_namespaces};
use crate::session::{ConnectionHealth, NamespaceWatchMessage, Session};
use crate::workspace::STALE_MESSAGE;

pub struct SessionView {
    session: Entity<Session>,
    table: Entity<TableState<NamespaceTable>>,
}

impl SessionView {
    pub fn new(cluster: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let session = cx.new(|cx| Session::new(cluster, cx));
        Self::with_session(session, window, cx)
    }

    pub fn from_events(
        cluster: String,
        receiver: mpsc::Receiver<NamespaceWatchMessage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = cx.new(|cx| Session::from_events(cluster, receiver, cx));
        Self::with_session(session, window, cx)
    }

    fn with_session(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table = cx.new(|cx| {
            TableState::new(NamespaceTable::default(), window, cx)
                .row_selectable(false)
                .col_selectable(false)
                .sortable(false)
                .col_movable(false)
        });
        cx.observe(&session, |this, session, cx| this.refresh(&session, cx))
            .detach();
        Self { session, table }
    }

    fn refresh(&mut self, session: &Entity<Session>, cx: &mut Context<Self>) {
        let namespaces = session.read(cx).namespaces();
        let rows = project_namespaces(&namespaces);
        self.table.update(cx, |table, cx| {
            table.delegate_mut().rows = rows;
            table.refresh(cx);
        });
        cx.notify();
    }

    pub fn health(&self, cx: &App) -> ConnectionHealth {
        self.session.read(cx).health()
    }
}

impl Render for SessionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stale = self.session.read(cx).show_stale_warning();
        v_flex()
            .size_full()
            .when(stale, |this| {
                this.child(
                    div()
                        .id("namespace-stale")
                        .role(Role::Alert)
                        .aria_label(STALE_MESSAGE)
                        .test_support()
                        .child(Alert::warning("namespace-stale-alert", STALE_MESSAGE).banner()),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(DataTable::new(&self.table).stripe(true)),
            )
    }
}

#[derive(Default)]
struct NamespaceTable {
    rows: Vec<ResourceRow>,
}

impl TableDelegate for NamespaceTable {
    fn columns_count(&self, _cx: &App) -> usize {
        2
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        match col_ix {
            0 => Column::new("name", "Name").width(px(360.)),
            _ => Column::new("status", "Status").width(px(160.)),
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let cell = self.rows[row_ix].cells[col_ix].clone();
        div()
            .id(format!("namespace-cell-{row_ix}-{col_ix}"))
            .aria_label(cell.clone())
            .test_support()
            .child(cell)
    }
}
