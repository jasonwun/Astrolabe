//! The Scale form; the panel's `run` sends what it asks for.

use astrolabe_kube::object::Operation;
use astrolabe_model::projection::ReplicaCounts;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::h_flex;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::*;

use super::{ResourceDetailPanel, button};

impl ResourceDetailPanel {
    pub(super) fn open_scale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&input, window, |panel, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = event {
                panel.apply_scale(cx);
            }
        })
        .detach();
        input.update(cx, |input, cx| input.focus(window, cx));
        self.scale = Some(input);
        cx.notify();
    }

    fn apply_scale(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.scale else {
            return;
        };
        let replicas = match input.read(cx).value().trim().parse::<i32>() {
            Ok(replicas) if replicas >= 0 => replicas,
            _ => return self.show_notice("Replicas must be a whole number, 0 or more", cx),
        };
        if self.run(|object| Operation::Scale { object, replicas }, cx) {
            self.scale = None;
        }
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
                    panel.scale = None;
                    cx.notify();
                })),
            )
    }
}
