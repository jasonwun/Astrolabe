//! The Logs tab (#17): one container's followed log in a display-only
//! terminal, so ANSI colours render as `kubectl logs` shows them.

use std::sync::Arc;

use astrolabe_kube::pod::LogStream;
use astrolabe_model::terminal_status::LogState;
use astrolabe_terminal::backend::display_only;
use astrolabe_terminal::{TerminalBackend, TerminalView, terminal_bytes};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::*;

use crate::terminal_panel::{COLS, ROWS, SCROLLBACK, terminal_tab};

pub struct LogsPanel {
    title: String,
    terminal: Entity<TerminalView>,
    state: LogState,
    _stream: Task<()>,
}

impl LogsPanel {
    pub fn new(
        title: String,
        stream: LogStream,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (terminal, task) = Self::follow(stream, window, cx);
        Self {
            title,
            terminal,
            state: LogState::Streaming,
            _stream: task,
        }
    }

    /// The stream has ended or failed.
    pub fn is_finished(&self) -> bool {
        self.state.is_finished()
    }

    /// Follow `stream` in this tab, on a fresh terminal.
    pub fn restart(&mut self, stream: LogStream, window: &mut Window, cx: &mut Context<Self>) {
        let (terminal, task) = Self::follow(stream, window, cx);
        self.terminal = terminal;
        self._stream = task;
        self.state = LogState::Streaming;
        cx.notify();
    }

    fn follow(
        mut stream: LogStream,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TerminalView>, Task<()>) {
        let (backend, events) = display_only(COLS, ROWS, SCROLLBACK);
        let backend: Arc<dyn TerminalBackend> = Arc::new(backend);
        let terminal = {
            let backend = backend.clone();
            cx.new(|cx| TerminalView::new(window, cx, backend, Arc::new(events)))
        };
        let task = cx.spawn(async move |panel, cx| {
            let state = loop {
                match stream.next().await {
                    Some(Ok(chunk)) => backend.feed_output(&terminal_bytes(&chunk)),
                    Some(Err(error)) => break LogState::Failed(error),
                    None => break LogState::Ended,
                }
            };
            panel
                .update(cx, |panel, cx| {
                    panel.state = state;
                    cx.notify();
                })
                .ok();
        });
        (terminal, task)
    }
}

impl EventEmitter<PanelEvent> for LogsPanel {}

impl Focusable for LogsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.focus_handle(cx)
    }
}

impl BasePanel for LogsPanel {
    fn panel_name(&self) -> &'static str {
        "Logs"
    }
}

impl Panel for LogsPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title.clone().into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title.clone()
    }
}

impl Render for LogsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        terminal_tab(
            "logs-tab",
            &self.title,
            self.state.notice(),
            &self.terminal,
            cx,
        )
    }
}
