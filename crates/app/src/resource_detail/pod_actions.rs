//! The Pod action flow: Logs, Exec and Port Forward, with the picker that
//! asks for a container, a shell or a port, and the session's forwards.

use astrolabe_kube::pod::ForwardTarget;
use astrolabe_model::detail_actions::{
    Picking, PodAction, SHELLS, containers, forward_row, picker_empty, picker_empty_notice,
};
use astrolabe_model::notice::Intent;
use astrolabe_model::projection::{DetailModel, Forward};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::notice_view::{intent_color, notice_view};

use super::{ResourceDetailPanel, button};

impl ResourceDetailPanel {
    fn choose_container(&mut self, action: PodAction, container: String, cx: &mut Context<Self>) {
        let effect = self
            .actions
            .choose_container(action, container, &self.subject(cx));
        self.carry_out(effect, cx);
    }

    fn choose_shell(&mut self, container: String, shell: &str, cx: &mut Context<Self>) {
        let effect = self
            .actions
            .choose_shell(container, shell, &self.subject(cx));
        self.carry_out(effect, cx);
    }

    fn choose_port(&mut self, port: u16, cx: &mut Context<Self>) {
        let effect = self.actions.choose_port(port, &self.subject(cx));
        self.carry_out(effect, cx);
    }

    pub(super) fn render_picker(
        &self,
        picking: Picking,
        model: &DetailModel,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let prompt = picking.prompt();
        let reason = picker_empty(model, &picking);
        let Picking { action, container } = picking;
        let choices: Vec<AnyElement> = match (action, container) {
            (PodAction::Forward, _) => model
                .forward
                .as_ref()
                .map(|forward| self.render_ports(forward, cx))
                .unwrap_or_default(),
            (_, None) if let Some(text) = reason => vec![self.render_empty(text, cx)],
            (_, None) => containers(model, action)
                .into_iter()
                .map(|container| {
                    button(
                        SharedString::from(format!("pick-container-{container}")),
                        container.clone(),
                        border,
                    )
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        panel.choose_container(action, container.clone(), cx)
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
                        panel.choose_shell(container.clone(), shell, cx)
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
                        panel.actions.cancel_picker();
                        cx.notify();
                    },
                )),
            )
    }

    /// The muted reason the picker has nothing to offer.
    fn render_empty(&self, text: &'static str, cx: &mut Context<Self>) -> AnyElement {
        notice_view(picker_empty_notice(text), cx)
            .text_sm()
            .into_any_element()
    }

    /// One button per declared port (a Service's TCP ports), or why there
    /// are none.
    fn render_ports(&self, forward: &Forward, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let border = cx.theme().border;
        let ports = forward.ports.clone();
        if ports.is_empty() {
            return vec![self.render_empty(forward.empty, cx)];
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
                .on_click(cx.listener(move |panel, _, _, cx| panel.choose_port(number, cx)))
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
            .state()
            .forwards(&target)
            .map(|forward| {
                let request = forward.request.clone();
                let port = request.port;
                let (text, tone) = forward_row(&forward.state, port);
                let color = intent_color(tone, theme);
                let failed = tone == Intent::Danger;
                let last_error = forward.last_error.as_ref().map(|error| {
                    let text = format!("Last connection failed: {error}");
                    div()
                        .id(SharedString::from(format!("forward-error-{port}")))
                        .aria_label(text.clone())
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(text)
                });
                // The texts wrap in the width Stop leaves them (`min_w_0`
                // lets a flex child shrink below its text); Stop keeps its
                // size and stays in the window (#213).
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .id(SharedString::from(format!("forward-{port}")))
                            .aria_label(text.clone())
                            .min_w_0()
                            // A Failed status is the long text and wraps;
                            // the others keep their width.
                            .map(|status| {
                                if failed {
                                    status.flex_1()
                                } else {
                                    status.flex_none()
                                }
                            })
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
                        .flex_none()
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
