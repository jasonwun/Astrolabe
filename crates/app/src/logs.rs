//! The Logs tab (#17): one container's followed log in a display-only
//! terminal, so ANSI colours render as `kubectl logs` shows them.

use std::sync::Arc;

use astrolabe_terminal::backend::display_only;
use astrolabe_terminal::{TerminalBackend, TerminalView};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::v_flex;
use gpui_kit::*;

use crate::session::LogStream;

/// Initial grid; the view resizes it to the tab.
const COLS: usize = 160;
const ROWS: usize = 40;
const SCROLLBACK: usize = 10_000;

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

#[derive(Clone, Debug, PartialEq, Eq)]
enum StreamState {
    Streaming,
    Ended,
    Failed(String),
}

pub struct LogsPanel {
    title: String,
    terminal: Entity<TerminalView>,
    state: StreamState,
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
                    Some(Err(error)) => break StreamState::Failed(error),
                    None => break StreamState::Ended,
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
            state: StreamState::Streaming,
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
        let theme = cx.theme();
        let (status, color) = match &self.state {
            StreamState::Streaming => ("Streaming…".to_string(), theme.muted_foreground),
            StreamState::Ended => ("Stream ended".to_string(), theme.muted_foreground),
            StreamState::Failed(error) => (format!("Stream failed: {error}"), theme.danger),
        };
        v_flex()
            .id("logs-tab")
            .aria_label(self.title.clone())
            .test_support()
            .size_full()
            .child(
                div()
                    .id("logs-status")
                    .aria_label(status.clone())
                    .test_support()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(color)
                    .child(status),
            )
            .child(div().flex_1().min_h_0().child(self.terminal.clone()))
    }
}
