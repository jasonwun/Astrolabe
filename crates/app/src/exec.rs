//! The Exec tab (#17): an interactive shell in one container, rendered by
//! a remote terminal whose input and size go to the exec session.

use std::sync::Arc;

use astrolabe_terminal::backend::{AlacrittyBackend, ExitStatus, RemoteSink, remote};
use astrolabe_terminal::{TerminalBackend, TerminalView};
use futures::channel::mpsc;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::v_flex;
use gpui_kit::*;

use crate::session::{ExecInput, ExecOutput, ExecSession};

/// Initial grid; the view resizes it to the tab.
const COLS: usize = 160;
const ROWS: usize = 40;
const SCROLLBACK: usize = 10_000;

/// The terminal's far end: the exec session's input.
struct SessionSink(mpsc::UnboundedSender<ExecInput>);

impl RemoteSink for SessionSink {
    fn input(&self, bytes: &[u8]) {
        let _ = self.0.unbounded_send(ExecInput::Bytes(bytes.to_vec()));
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.0.unbounded_send(ExecInput::Resize { cols, rows });
    }

    fn close(&self) {
        self.0.close_channel();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SessionState {
    Running,
    Exited(u8),
    Failed(String),
}

pub struct ExecPanel {
    title: String,
    terminal: Entity<TerminalView>,
    state: SessionState,
    _session: Task<()>,
}

impl ExecPanel {
    pub fn new(
        title: String,
        mut session: ExecSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (backend, events) = remote(
            COLS,
            ROWS,
            SCROLLBACK,
            Box::new(SessionSink(session.input())),
        );
        let backend: Arc<AlacrittyBackend> = Arc::new(backend);
        let terminal = {
            let backend: Arc<dyn TerminalBackend> = backend.clone();
            cx.new(|cx| TerminalView::new(window, cx, backend, Arc::new(events)))
        };
        let task = cx.spawn(async move |panel, cx| {
            let state = loop {
                match session.next().await {
                    Some(ExecOutput::Bytes(bytes)) => backend.feed_output(&bytes),
                    Some(ExecOutput::Exited(Ok(code))) => {
                        backend.remote_exited(ExitStatus {
                            code: Some(code.into()),
                            signal: None,
                        });
                        break SessionState::Exited(code);
                    }
                    Some(ExecOutput::Exited(Err(error))) => break SessionState::Failed(error),
                    None => break SessionState::Failed("session dropped".to_string()),
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
            state: SessionState::Running,
            _session: task,
        }
    }
}

impl EventEmitter<PanelEvent> for ExecPanel {}

impl Focusable for ExecPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.focus_handle(cx)
    }
}

impl BasePanel for ExecPanel {
    fn panel_name(&self) -> &'static str {
        "Exec"
    }
}

impl Panel for ExecPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title.clone().into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title.clone()
    }
}

impl Render for ExecPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (status, color) = match &self.state {
            SessionState::Running => ("Session running".to_string(), theme.muted_foreground),
            SessionState::Exited(code) => {
                (format!("Exited with code {code}"), theme.muted_foreground)
            }
            SessionState::Failed(error) => (format!("Session failed: {error}"), theme.danger),
        };
        v_flex()
            .id("exec-tab")
            .aria_label(self.title.clone())
            .test_support()
            .size_full()
            .child(
                div()
                    .id("exec-status")
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
