//! The workload toolbar actions: Scale, Rollout Restart, Suspend/Resume and
//! Trigger.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::h_flex;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::*;

use super::{ResourceDetailPanel, button};
use crate::projection::ReplicaCounts;
use crate::session::Operation;

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
        let Some(resolved) = self.resolve(cx) else {
            return;
        };
        let operation = Operation::Scale {
            object: resolved.object,
            replicas,
        };
        let result = self
            .session
            .update(cx, |session, _| session.operate(operation));
        self.scale = None;
        self.report(
            result,
            "Scaling…",
            format!("Scaled to {replicas}"),
            "Scale",
            cx,
        );
    }

    pub(super) fn rollout_restart(&mut self, cx: &mut Context<Self>) {
        let Some(resolved) = self.resolve(cx) else {
            return;
        };
        let operation = Operation::RolloutRestart {
            object: resolved.object,
        };
        let result = self
            .session
            .update(cx, |session, _| session.operate(operation));
        self.report(
            result,
            "Restarting…",
            "Restart requested".to_string(),
            "Restart",
            cx,
        );
    }

    pub(super) fn set_suspend(&mut self, suspend: bool, cx: &mut Context<Self>) {
        let Some(resolved) = self.resolve(cx) else {
            return;
        };
        let operation = Operation::SetSuspend {
            object: resolved.object,
            suspend,
        };
        let result = self
            .session
            .update(cx, |session, _| session.operate(operation));
        let (pending, done, verb) = if suspend {
            ("Suspending…", "Suspended", "Suspend")
        } else {
            ("Resuming…", "Resumed", "Resume")
        };
        self.report(result, pending, done.to_string(), verb, cx);
    }

    pub(super) fn trigger(&mut self, cx: &mut Context<Self>) {
        let Some(resolved) = self.resolve(cx) else {
            return;
        };
        let operation = Operation::TriggerCronJob {
            object: resolved.object,
        };
        let result = self
            .session
            .update(cx, |session, _| session.operate(operation));
        self.report(
            result,
            "Triggering…",
            "Job creation requested".to_string(),
            "Trigger",
            cx,
        );
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
