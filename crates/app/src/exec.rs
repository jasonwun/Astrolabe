//! The Exec tab (#17): an interactive shell in one container, rendered by
//! a remote terminal whose input and size go to the exec session.

use std::sync::Arc;

use astrolabe_kube::pod::{ExecInput, ExecOutput, ExecSession};
use astrolabe_model::terminal_status::ExecState;
use astrolabe_terminal::backend::{AlacrittyBackend, ExitStatus, RemoteSink, remote};
use astrolabe_terminal::{TerminalBackend, TerminalView};
use futures::channel::mpsc;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::*;

use crate::terminal_panel::{COLS, ROWS, SCROLLBACK, terminal_tab};

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

pub struct ExecPanel {
    title: String,
    terminal: Entity<TerminalView>,
    state: ExecState,
    _session: Task<()>,
}

impl ExecPanel {
    pub fn new(
        title: String,
        session: ExecSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (terminal, task) = Self::run(session, window, cx);
        Self {
            title,
            terminal,
            state: ExecState::Running,
            _session: task,
        }
    }

    /// The shell has exited or failed.
    pub fn is_finished(&self) -> bool {
        self.state.is_finished()
    }

    /// Run `session` in this tab, on a fresh terminal.
    pub fn restart(&mut self, session: ExecSession, window: &mut Window, cx: &mut Context<Self>) {
        let (terminal, task) = Self::run(session, window, cx);
        self.terminal = terminal;
        self._session = task;
        self.state = ExecState::Running;
        cx.notify();
    }

    fn run(
        mut session: ExecSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TerminalView>, Task<()>) {
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
                        break ExecState::Exited(code);
                    }
                    Some(ExecOutput::Exited(Err(error))) => {
                        break ExecState::Failed(error);
                    }
                    None => break ExecState::dropped(),
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
        terminal_tab(
            "exec-tab",
            &self.title,
            self.state.notice(),
            &self.terminal,
            cx,
        )
    }
}
