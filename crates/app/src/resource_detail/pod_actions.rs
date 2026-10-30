//! The Pod action flow: Logs, Exec and Port Forward, with the picker that
//! asks for a container, a shell or a port, and the session's forwards.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use super::{DetailTarget, OpenExec, OpenLogs, ResourceDetailPanel, button};
use crate::projection::{DetailModel, Forward, ForwardTo};
use crate::session::{ExecRequest, ForwardRequest, ForwardState, ForwardTarget, LogRequest};
use crate::store::ObjectKey;

/// A Pod action that runs in one container; Forward also serves Services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PodAction {
    Logs,
    Exec,
    /// Picks a port, not a container.
    Forward,
}

/// What the picker asks: a container, or for Exec in a chosen container,
/// a shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Picking {
    action: PodAction,
    container: Option<String>,
}

/// The shells Exec offers; `sh` exists in nearly every image.
const SHELLS: &[&str] = &["sh", "bash"];

impl ResourceDetailPanel {
    /// Run `action` in the only container, or ask which one.
    pub(super) fn pick_container(&mut self, action: PodAction, cx: &mut Context<Self>) {
        let containers = self
            .resolve(cx)
            .map(|resolved| containers(&resolved.model, action))
            .unwrap_or_default();
        match containers.as_slice() {
            [only] => self.run_in(action, only.clone(), cx),
            _ => self.ask(action, None, cx),
        }
    }

    pub(super) fn ask(
        &mut self,
        action: PodAction,
        container: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.picking = Some(Picking { action, container });
        cx.notify();
    }

    fn run_in(&mut self, action: PodAction, container: String, cx: &mut Context<Self>) {
        let DetailTarget::Builtin { key, .. } = &self.target else {
            return;
        };
        match action {
            PodAction::Logs => {
                self.picking = None;
                cx.notify();
                cx.emit(OpenLogs {
                    title: format!("Logs {key} ({container})"),
                    request: LogRequest {
                        pod: key.clone(),
                        container,
                    },
                })
            }
            PodAction::Exec => self.ask(action, Some(container), cx),
            PodAction::Forward => {}
        }
    }

    fn forward(&mut self, port: u16, cx: &mut Context<Self>) {
        self.picking = None;
        cx.notify();
        let Some(target) = self
            .resolve(cx)
            .and_then(|resolved| forward_target(&resolved.model, &resolved.object.key))
        else {
            return;
        };
        let request = ForwardRequest { target, port };
        self.session
            .update(cx, |session, cx| session.forward_port(request, cx));
    }

    fn exec(&mut self, container: String, shell: &str, cx: &mut Context<Self>) {
        self.picking = None;
        cx.notify();
        let DetailTarget::Builtin { key, .. } = &self.target else {
            return;
        };
        cx.emit(OpenExec {
            title: format!("Exec {key} ({container})"),
            request: ExecRequest {
                pod: key.clone(),
                container,
                shell: shell.to_string(),
            },
        });
    }

    pub(super) fn render_picker(
        &self,
        picking: Picking,
        model: &DetailModel,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let Picking { action, container } = picking;
        let prompt = match (action, &container) {
            (PodAction::Logs, _) => "Logs: choose a container".to_string(),
            (PodAction::Exec, None) => "Exec: choose a container".to_string(),
            (PodAction::Exec, Some(container)) => format!("Exec in {container}: choose a shell"),
            (PodAction::Forward, _) => "Port forward: choose a port".to_string(),
        };
        let choices: Vec<AnyElement> =
            match (action, container) {
                (PodAction::Forward, _) => model
                    .forward
                    .as_ref()
                    .map(|forward| self.render_ports(forward, cx))
                    .unwrap_or_default(),
                (_, None) => containers(model, action)
                    .into_iter()
                    .map(|container| {
                        button(
                            SharedString::from(format!("pick-container-{container}")),
                            container.clone(),
                            border,
                        )
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel.run_in(action, container.clone(), cx)
                        }))
                        .into_any_element()
                    })
                    .collect(),
                (_, Some(container)) => SHELLS
                    .iter()
                    .map(|shell| {
                        let container = container.clone();
                        button(
                            SharedString::from(format!("pick-shell-{shell}")),
                            *shell,
                            border,
                        )
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel.exec(container.clone(), shell, cx)
                        }))
                        .into_any_element()
                    })
                    .collect(),
            };
        h_flex()
            .id("container-picker")
            .test_support()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .id("picker-prompt")
                    .aria_label(prompt.clone())
                    .test_support()
                    .text_sm()
                    .child(prompt),
            )
            .children(choices)
            .child(
                button("picker-cancel", "Cancel", border).on_click(cx.listener(
                    |panel, _, _, cx| {
                        panel.picking = None;
                        cx.notify();
                    },
                )),
            )
    }

    /// One button per declared port (a Service's TCP ports), or why there
    /// are none.
    fn render_ports(&self, forward: &Forward, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let border = cx.theme().border;
        let ports = forward.ports.clone();
        if ports.is_empty() {
            let text = forward.empty;
            return vec![
                div()
                    .id("picker-empty")
                    .aria_label(text)
                    .test_support()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(text)
                    .into_any_element(),
            ];
        }
        ports
            .into_iter()
            .map(|port| {
                let number = port.port;
                button(
                    SharedString::from(format!("pick-port-{number}")),
                    port.label,
                    border,
                )
                .on_click(cx.listener(move |panel, _, _, cx| panel.forward(number, cx)))
                .into_any_element()
            })
            .collect()
    }

    /// The session's forwards to the shown Pod or Service, each with Stop.
    pub(super) fn render_forwards(
        &self,
        target: ForwardTarget,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.theme();
        let session = self.session.read(cx);
        let rows: Vec<AnyElement> = session
            .forwards(&target)
            .map(|forward| {
                let request = forward.request.clone();
                let port = request.port;
                let (text, color) = match &forward.state {
                    ForwardState::Starting => {
                        (format!("Starting → {port}…"), theme.muted_foreground)
                    }
                    ForwardState::Listening(local) => {
                        (format!("localhost:{local} → {port}"), theme.foreground)
                    }
                    ForwardState::Failed(error) => {
                        (format!("Forward to {port} failed: {error}"), theme.danger)
                    }
                };
                let last_error = forward.last_error.as_ref().map(|error| {
                    let text = format!("Last connection failed: {error}");
                    div()
                        .id(SharedString::from(format!("forward-error-{port}")))
                        .aria_label(text.clone())
                        .test_support()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(text)
                });
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id(SharedString::from(format!("forward-{port}")))
                            .aria_label(text.clone())
                            .test_support()
                            .text_sm()
                            .text_color(color)
                            .child(text),
                    )
                    .children(last_error)
                    .child(
                        button(
                            SharedString::from(format!("forward-stop-{port}")),
                            "Stop",
                            theme.border,
                        )
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel
                                .session
                                .update(cx, |session, cx| session.stop_forward(&request, cx))
                        })),
                    )
                    .into_any_element()
            })
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .id("detail-forwards")
                .test_support()
                .gap_1()
                .p_2()
                .border_b_1()
                .border_color(theme.border)
                .children(rows)
                .into_any_element(),
        )
    }
}

/// The containers `action` can target: Logs reads every container, init
/// containers first; Exec needs one still running.
fn containers(model: &DetailModel, action: PodAction) -> Vec<String> {
    let Some(containers) = &model.containers else {
        return Vec::new();
    };
    match action {
        PodAction::Logs => containers.all.clone(),
        PodAction::Exec | PodAction::Forward => containers.running.clone(),
    }
}

/// What Port Forward reaches from the detail of the object at `key`.
pub(super) fn forward_target(model: &DetailModel, key: &ObjectKey) -> Option<ForwardTarget> {
    Some(match model.forward.as_ref()?.to {
        ForwardTo::Pod => ForwardTarget::Pod(key.clone()),
        ForwardTo::Service => ForwardTarget::Service(key.clone()),
    })
}
