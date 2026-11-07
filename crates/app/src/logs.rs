//! The Logs tab (#17): one container's followed log in a display-only
//! terminal, so ANSI colours render as `kubectl logs` shows them.

use std::sync::Arc;

use astrolabe_kube::pod::LogStream;
use astrolabe_model::terminal_status::LogState;
use astrolabe_terminal::backend::display_only;
use astrolabe_terminal::{TerminalBackend, TerminalView};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::*;

use crate::terminal_panel::{COLS, ROWS, SCROLLBACK, terminal_tab};

/// Log bytes as a terminal must receive them: a bare `\n` only moves the
/// cursor down, so each gets the carriage return a TTY would add. A `\r`
/// ending one chunk before the next chunk's `\n` gains a harmless second
/// `\r`.
pub fn terminal_bytes(chunk: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(chunk.len() + chunk.len() / 16);
    for (index, byte) in chunk.iter().enumerate() {
        if *byte == b'\n' && (index == 0 || chunk[index - 1] != b'\r') {
            bytes.push(b'\r');
        }
        bytes.push(*byte);
    }
    bytes
}

pub struct LogsPanel {
    title: String,
    terminal: Entity<TerminalView>,
    state: LogState,
    _stream: Task<()>,
}

impl LogsPanel {
    pub fn new(
        title: String,
        mut stream: LogStream,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        Self {
            title,
            terminal,
            state: LogState::Streaming,
            _stream: task,
        }
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
