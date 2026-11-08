//! The Scale form's rendering; `DetailActions` decides what it sends.

use astrolabe_model::projection::ReplicaCounts;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::h_flex;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::*;

use super::{ResourceDetailPanel, button};

impl ResourceDetailPanel {
    /// The one place the Scale input follows `DetailActions::scale_open`:
    /// created (and focused) when the form opens, dropped when it closes.
    pub(super) fn sync_scale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (self.actions.scale_open(), self.scale.is_some()) {
            (true, false) => {
                let input = cx.new(|cx| InputState::new(window, cx));
                cx.subscribe_in(&input, window, |panel, _, event: &InputEvent, _, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        panel.apply_scale(cx);
                    }
                })
                .detach();
                input.update(cx, |input, cx| input.focus(window, cx));
                self.scale = Some(input);
            }
            (false, true) => self.scale = None,
            _ => {}
        }
    }

    fn apply_scale(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.scale else {
            return;
        };
        let text = input.read(cx).value().to_string();
        let effect = self.actions.submit_scale(&text, &self.subject(cx));
        self.carry_out(effect, cx);
    }

    pub(super) fn render_scale(
        &self,
        input: &Entity<InputState>,
        replicas: Option<ReplicaCounts>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let label = match replicas {
            Some(counts) => format!("Replicas (now {})", counts.desired),
            None => "Replicas".to_string(),
        };
        let border = theme.border;
        let button = |id: &'static str, label: &'static str| button(id, label, border);
        h_flex()
            .id("scale-form")
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("scale-label")
                    .aria_label(label.clone())
                    .test_support()
                    .text_sm()
                    .child(label),
            )
            .child(Input::new(input).id("scale-input").w(px(120.)))
            .child(
                button("scale-apply", "Apply")
                    .on_click(cx.listener(|panel, _, _, cx| panel.apply_scale(cx))),
            )
            .child(
                button("scale-cancel", "Cancel").on_click(cx.listener(|panel, _, _, cx| {
                    panel.actions.cancel_scale();
                    cx.notify();
                })),
            )
    }
}
