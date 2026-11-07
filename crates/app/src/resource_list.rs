//! The list tab: one fixed center-dock panel whose content follows the
//! navigation selection (Namespaces, a built-in kind, CRD Definitions, or a
//! CRD kind).

use std::collections::BTreeSet;

use astrolabe_kube::object::ObjectKey;
use astrolabe_model::projection::{ResourceTable, track_ages};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::table::TableState;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::clock;
use crate::notice_view::notice_view;
use crate::perf;
use crate::rows_table::{RowsTable, fitted_table, open_row_with, rows_table_state};
use crate::session::Session;
use astrolabe_model::detail::DetailTarget;
use astrolabe_model::list::{self, ListOutcome};
use astrolabe_model::notice::Notice;
use astrolabe_model::session::StaleCause;
use astrolabe_model::session_state::Viewing;

/// A row was clicked: open its detail tab.
pub struct OpenDetail(pub DetailTarget);

pub struct ResourceListPanel {
    session: Entity<Session>,
    table: Entity<TableState<RowsTable>>,
    /// Why the table is not shown, if it is not.
    outcome_notice: Option<Notice>,
    stale: Option<StaleCause>,
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
            table,
            outcome_notice: None,
            stale: None,
            group_filter: BTreeSet::new(),
            notice: None,
            tick: Task::ready(()),
            focus_handle: cx.focus_handle(),
        };
        panel.refresh(cx);
        panel
    }

    /// Opens `viewing` in the list tab.
    pub fn show(&mut self, viewing: Viewing, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| match &viewing {
            Viewing::Builtin(kind) => {
                session.open_builtin_list(*kind, cx);
            }
            Viewing::Custom(id) => {
                session.open_custom_list(id, cx);
            }
            Viewing::Namespaces => session.open_namespaces_list(),
            Viewing::Definitions => session.open_definitions_list(),
        });
        self.notice = None;
        self.refresh(cx);
    }

    fn open_row(&mut self, key: &ObjectKey, cx: &mut Context<Self>) {
        let state = self.session.read(cx).state();
        match DetailTarget::from_row(state.viewing(), key, state) {
            Ok(target) => {
                self.notice = None;
                cx.emit(OpenDetail(target));
            }
            Err(error) => {
                self.notice = Some(error.to_string());
                cx.notify();
            }
        }
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
        let outcome_notice = resolved.outcome.notice();
        let table = match resolved.outcome {
            ListOutcome::Rows(table) => table,
            _ => ResourceTable::default(),
        };
        let stale = resolved.stale;
        self.outcome_notice = outcome_notice;
        self.stale = stale;
        self.tick = cx.spawn(async move |panel, cx| {
            cx.background_executor().timer(next_tick).await;
            panel.update(cx, |panel, cx| panel.refresh(cx)).ok();
        });
        let open_all_cells = match data.viewing() {
            Viewing::Builtin(kind) => kind.registration().open_all_cells,
            _ => false,
        };
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
        let groups = self.session.read(cx).state().crd_groups();
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
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        let state = self.session.read(cx).state();
        Some(state.viewing().label(state).into())
    }

    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tab_name(cx).unwrap_or_default()
    }
}

impl Render for ResourceListPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            // The dock focuses the displayed panel; `cmd-w` dispatches from
            // here.
            .track_focus(&self.focus_handle)
            .size_full()
            .when_some(self.stale, |this, cause| {
                // Distinct ids and texts per cause keep `try_find`
                // unambiguous; the alert takes `(id, 0)`: a tuple id needs a
                // number.
                let Notice { id, text, .. } = cause.notice();
                this.child(
                    div().id(id).aria_label(text.clone()).test_support().child(
                        gpui_kit::component::alert::Alert::warning((id, 0usize), text).banner(),
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
            .when(
                matches!(
                    self.session.read(cx).state().viewing(),
                    Viewing::Definitions
                ),
                |this| this.child(self.render_group_filter(cx)),
            )
            .map(|this| match self.outcome_notice.clone() {
                Some(notice) => this.child(notice_view(notice, cx).p_4()),
                None => this.child(fitted_table(&self.table, "list-table").flex_1().min_h_0()),
            })
    }
}
