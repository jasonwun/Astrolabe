//! What the Logs and Exec tabs share (#304): the terminal's initial grid and
//! the render shell, a status bar over the terminal.

use astrolabe_model::notice::Notice;
use astrolabe_terminal::TerminalView;
use gpui_kit::component::v_flex;
use gpui_kit::*;

use crate::notice_view::notice_view;

/// Initial grid; the view resizes it to the tab.
pub(crate) const COLS: usize = 160;
pub(crate) const ROWS: usize = 40;
pub(crate) const SCROLLBACK: usize = 10_000;

/// The tab `tab_id` titled `title`: `status` over `terminal`.
pub(crate) fn terminal_tab(
    tab_id: &'static str,
    title: &str,
    status: Notice,
    terminal: &Entity<TerminalView>,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .id(tab_id)
        .aria_label(title.to_string())
        .test_support()
        .size_full()
        .child(notice_view(status, cx).px_2().py_1().text_xs())
        .child(div().flex_1().min_h_0().child(terminal.clone()))
}
