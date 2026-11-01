//! Generic detail tab (#11): metadata summary, status summary, read-only
//! YAML; toolbar Edit YAML (separate editor tab, SSA) and Delete. It reads
//! the object out of the list watch's snapshot, so it refreshes with the watch
//! and issues no GET/watch of its own. Built-in kinds reuse this template
//! through their registration (#17).

use std::collections::BTreeSet;
use std::rc::Rc;

use gpui_kit::base::test_support::Observed;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::input::InputState;
use gpui_kit::component::table::TableState;
use gpui_kit::component::{Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tokio::sync::oneshot;

use crate::builtin::Action;
use crate::clock;
use crate::detail::{self, DetailTarget, RelatedOutcome, RelatedRows, Resolved, watches};
use crate::projection::config::{HIDDEN, SecretKey};
use crate::projection::{DetailField, DetailItem, ReplicaCounts, SummaryTable, track_ages};
use crate::resource_list::{OpenDetail, OpenRow, RowsTable, fitted_table, rows_table_state};
use crate::session::{ExecRequest, LogRequest, Session};
use crate::store::ObjectKey;

mod editor_panel;
mod pod_actions;
mod workload_actions;

pub use editor_panel::EditorPanel;
use pod_actions::{Picking, PodAction, forward_target};

/// Logs was pressed: open a Logs tab following this container.
pub struct OpenLogs {
    pub title: String,
    pub request: LogRequest,
}

/// A shell was chosen: open an Exec tab running it.
pub struct OpenExec {
    pub title: String,
    pub request: ExecRequest,
}

/// Edit YAML was pressed: open an editor tab on this text.
pub struct OpenEditor {
    pub title: String,
    pub yaml: String,
}

pub struct ResourceDetailPanel {
    session: Entity<Session>,
    target: DetailTarget,
    confirm_delete: bool,
    /// The Scale field, while open.
    scale: Option<Entity<InputState>>,
    /// The picker, while a Pod action waits for a container, a shell or a
    /// port.
    picking: Option<Picking>,
    /// The Secret keys whose values are shown; every other value stays
    /// masked.
    revealed: BTreeSet<String>,
    notice: Option<String>,
    /// The Related section's table; empty when the target's kind has none.
    related_table: Entity<TableState<RowsTable>>,
    /// Re-renders when the youngest shown age next changes (#70); replaced
    /// on every render, and dropped with the panel.
    tick: Task<()>,
    focus_handle: FocusHandle,
}

impl ResourceDetailPanel {
    pub fn new(
        session: Entity<Session>,
        target: DetailTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let needed = watches(&target);
        session.update(cx, |session, cx| {
            for watch in &needed {
                session.watch_builtin(watch.kind, &watch.namespace, cx);
            }
        });
        cx.observe(&session, |panel, session, cx| {
            // A preferred version change needs the new version's watch, even
            // with the kind's list closed.
            if let DetailTarget::Custom { id, key } = &panel.target {
                session.update(cx, |session, cx| {
                    session.watch_custom_object(id, key.namespace.clone(), cx)
                });
            }
            cx.notify();
        })
        .detach();
        let open_row: OpenRow = {
            let panel = cx.entity().downgrade();
            Rc::new(move |key: &ObjectKey, cx: &mut App| {
                panel
                    .update(cx, |panel, cx| panel.open_related(key, cx))
                    .ok();
            })
        };
        let related_table = cx.new(|cx| rows_table_state(RowsTable::related(open_row), window, cx));
        Self {
            session,
            target,
            confirm_delete: false,
            scale: None,
            picking: None,
            revealed: BTreeSet::new(),
            notice: None,
            related_table,
            tick: Task::ready(()),
            focus_handle: cx.focus_handle(),
        }
    }

    /// A Related row's first cell was clicked: open that object's detail.
    fn open_related(&mut self, key: &ObjectKey, cx: &mut Context<Self>) {
        let DetailTarget::Builtin { kind, .. } = &self.target else {
            return;
        };
        if let Some(related) = &kind.registration().related {
            cx.emit(OpenDetail(DetailTarget::Builtin {
                kind: related.kind,
                key: key.clone(),
            }));
        }
    }

    fn resolve(&self, cx: &App) -> Option<Resolved> {
        detail::resolve(&self.target, self.session.read(cx), clock::now(cx))
    }

    fn display_name(&self) -> String {
        match &self.target {
            DetailTarget::Namespace { name } => format!("Namespace {name}"),
            DetailTarget::Definition { name } => name.clone(),
            DetailTarget::Builtin { kind, key } => format!("{} {key}", kind.kind()),
            DetailTarget::Custom { id, key } => format!("{} {key}", id.kind),
        }
    }

    fn edit(&mut self, cx: &mut Context<Self>) {
        if let Some(resolved) = self.resolve(cx) {
            cx.emit(OpenEditor {
                title: resolved.model.title,
                yaml: resolved.model.edit_yaml.unwrap_or(resolved.model.yaml),
            });
        }
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        self.confirm_delete = false;
        let Some(resolved) = self.resolve(cx) else {
            return;
        };
        let result = self
            .session
            .update(cx, |session, _| session.delete(resolved.object));
        self.report(
            result,
            "Deleting…",
            "Deletion requested".to_string(),
            "Delete",
            cx,
        );
    }

    /// A Secret's keys, each value masked until revealed.
    fn render_secret_keys(&self, keys: &[SecretKey], cx: &mut Context<Self>) -> Option<AnyElement> {
        if keys.is_empty() {
            return None;
        }
        let border = cx.theme().border;
        let rows = keys.iter().map(|entry| {
            let key = entry.key.clone();
            let revealed = self.revealed.contains(&key);
            let value = if revealed {
                entry.value.reveal().to_string()
            } else {
                format!("{HIDDEN} ({} bytes)", entry.bytes)
            };
            let toggle = if revealed {
                button(
                    SharedString::from(format!("secret-hide-{key}")),
                    "Hide",
                    border,
                )
            } else {
                button(
                    SharedString::from(format!("secret-reveal-{key}")),
                    "Reveal",
                    border,
                )
            };
            h_flex()
                .gap_2()
                .child(field(&key, &value))
                .child(toggle.on_click(cx.listener(move |panel, _, _, cx| {
                    if !panel.revealed.remove(&key) {
                        panel.revealed.insert(key.clone());
                    }
                    cx.notify();
                })))
                .into_any_element()
        });
        Some(
            v_flex()
                .id("detail-secret-data")
                .test_support()
                .gap_1()
                .pt_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Data"),
                )
                .children(rows.collect::<Vec<_>>())
                .into_any_element(),
        )
    }

    fn show_notice(&mut self, notice: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(notice.into());
        cx.notify();
    }

    /// Show `pending` until `result` resolves, then `done` or the failure
    /// of `verb`.
    fn report(
        &mut self,
        result: oneshot::Receiver<Result<(), String>>,
        pending: &str,
        done: String,
        verb: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.show_notice(pending, cx);
        cx.spawn(async move |panel, cx| {
            let result = result
                .await
                .unwrap_or_else(|_| Err(format!("{verb} task dropped")));
            panel
                .update(cx, |panel, cx| {
                    panel.show_notice(
                        match result {
                            Ok(()) => done,
                            Err(error) => format!("{verb} failed: {error}"),
                        },
                        cx,
                    )
                })
                .ok();
        })
        .detach();
    }

    fn render_toolbar(
        &self,
        actions: &[Action],
        suspended: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let button = |id: &'static str, label: &'static str| button(id, label, border);
        h_flex()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(theme.border)
            .children(actions.iter().map(|action| {
                match action {
                    Action::Logs => button("detail-logs", "Logs").on_click(
                        cx.listener(|panel, _, _, cx| panel.pick_container(PodAction::Logs, cx)),
                    ),
                    Action::Exec => button("detail-exec", "Exec").on_click(
                        cx.listener(|panel, _, _, cx| panel.pick_container(PodAction::Exec, cx)),
                    ),
                    Action::PortForward => button("detail-forward", "Port Forward").on_click(
                        cx.listener(|panel, _, _, cx| panel.ask(PodAction::Forward, None, cx)),
                    ),
                    Action::Scale => button("detail-scale", "Scale")
                        .on_click(cx.listener(|panel, _, window, cx| panel.open_scale(window, cx))),
                    Action::RolloutRestart => button("detail-restart", "Rollout Restart")
                        .on_click(cx.listener(|panel, _, _, cx| panel.rollout_restart(cx))),
                    Action::Suspend => button(
                        "detail-suspend",
                        if suspended { "Resume" } else { "Suspend" },
                    )
                    .on_click(
                        cx.listener(move |panel, _, _, cx| panel.set_suspend(!suspended, cx)),
                    ),
                    Action::Trigger => button("detail-trigger", "Trigger")
                        .on_click(cx.listener(|panel, _, _, cx| panel.trigger(cx))),
                    Action::Edit => button("detail-edit", "Edit YAML")
                        .on_click(cx.listener(|panel, _, _, cx| panel.edit(cx))),
                    Action::Delete => button("detail-delete", "Delete")
                        .text_color(theme.danger)
                        .on_click(cx.listener(|panel, _, _, cx| {
                            panel.confirm_delete = true;
                            cx.notify();
                        })),
                }
            }))
            .when_some(self.notice.clone(), |bar, notice| {
                bar.child(
                    div()
                        .id("detail-notice")
                        .aria_label(notice.clone())
                        .test_support()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(notice),
                )
            })
    }

    /// The related kind's list columns over the selected objects, fitted to
    /// the section's width by that kind's weights; a row's first cell opens
    /// its detail.
    fn render_related(&self, related: RelatedRows, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let content = match related.outcome {
            RelatedOutcome::Forbidden(text) | RelatedOutcome::Empty(text) => div()
                .id("related-empty")
                .aria_label(text.clone())
                .test_support()
                .text_sm()
                .text_color(muted)
                .child(text)
                .into_any_element(),
            RelatedOutcome::Rows(table) => {
                // Header plus rows, inside the table's 1px borders: the
                // detail body scrolls, the table never does vertically.
                let height =
                    Size::default().table_row_height() * (table.rows.len() + 1) as f32 + px(2.);
                self.related_table.update(cx, |state, cx| {
                    state.delegate_mut().set_table(table);
                    state.refresh(cx);
                });
                fitted_table(&self.related_table, "related-table")
                    .w_full()
                    .h(height)
                    .into_any_element()
            }
        };
        v_flex()
            .id("detail-related")
            .test_support()
            .gap_1()
            .p_2()
            .child(
                div()
                    .id("related-title")
                    .aria_label(related.title)
                    .test_support()
                    .text_xs()
                    .text_color(muted)
                    .child(related.title),
            )
            .child(content)
            .into_any_element()
    }

    fn render_confirm(&self, cascade: String, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let question = format!("Delete {}?", self.display_name());
        let action = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .aria_label(label)
                .test_support()
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .child(label)
        };
        v_flex()
            .id("delete-confirm")
            .test_support()
            .m_2()
            .p_3()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(theme.danger)
            .bg(theme.background)
            .child(div().child(question))
            .child(
                div()
                    .id("delete-cascade")
                    .aria_label(cascade.clone())
                    .test_support()
                    .text_sm()
                    .text_color(theme.warning)
                    .child(cascade),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        action("delete-confirm-yes", "Delete")
                            .text_color(theme.danger)
                            .on_click(cx.listener(|panel, _, _, cx| panel.delete(cx))),
                    )
                    .child(action("delete-confirm-no", "Cancel").on_click(cx.listener(
                        |panel, _, _, cx| {
                            panel.confirm_delete = false;
                            cx.notify();
                        },
                    ))),
            )
    }
}

fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    border: Hsla,
) -> Observed<Stateful<Div>> {
    let label = label.into();
    div()
        .id(id)
        .aria_label(label.clone())
        .test_support()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .text_sm()
        .child(label)
}

/// A fixed-width table cell labeled with its full text. The text ends in
/// an ellipsis inside the right gutter, so it never meets the next column
/// (#75); `<id>-text` is the text element.
fn table_cell(id: SharedString, text: String, width: f32) -> Observed<Stateful<Div>> {
    div()
        .id(id.clone())
        .aria_label(text.clone())
        .test_support()
        .w(px(width))
        .flex_shrink_0()
        .pr_2()
        .overflow_hidden()
        .whitespace_nowrap()
        .child(
            div()
                .id(SharedString::from(format!("{id}-text")))
                .test_support()
                .w_full()
                .text_ellipsis()
                .child(text),
        )
}

fn section(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    children: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .id(id)
        .test_support()
        .gap_1()
        .p_2()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(title.into()),
        )
        .children(children)
        .into_any_element()
}

fn field(label: &str, value: &str) -> AnyElement {
    let text = format!("{label}: {value}");
    div()
        .id(SharedString::from(format!("field-{label}")))
        .aria_label(text.clone())
        .test_support()
        .text_sm()
        .child(text)
        .into_any_element()
}

fn detail_field(field: &DetailField, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let text = format!(
        "{}: {}",
        field.label,
        field
            .values
            .iter()
            .map(|value| value.text.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    h_flex()
        .id(SharedString::from(format!("field-{}", field.label)))
        .aria_label(text)
        .test_support()
        .gap_1()
        .text_sm()
        .child(format!("{}:", field.label))
        .children(field.values.iter().enumerate().map(|(index, value)| {
            let item = div()
                .id(SharedString::from(format!("link-{}-{index}", field.label)))
                .aria_label(value.text.clone())
                .test_support()
                .child(value.text.clone());
            if let Some(href) = value.href.clone() {
                item.text_color(theme.info)
                    .underline()
                    .cursor_pointer()
                    .on_click(move |_, _, cx| cx.open_url(&href))
                    .into_any_element()
            } else {
                item.into_any_element()
            }
        }))
        .into_any_element()
}

fn list_field(label: &'static str, values: &[String]) -> Option<AnyElement> {
    (!values.is_empty()).then(|| field(label, &values.join(", ")))
}

/// Desired / updated / available / unavailable, as text and as a bar:
/// available, then updated but not yet available, then unavailable.
fn replica_bar(counts: ReplicaCounts, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let text = format!(
        "Replicas: {} desired, {} updated, {} available, {} unavailable",
        counts.desired, counts.updated, counts.available, counts.unavailable
    );
    let total = counts
        .desired
        .max(counts.available + counts.unavailable)
        .max(1) as f32;
    let width = 240.;
    let segment = |count: i32, color: Hsla| {
        div()
            .h_full()
            .w(px(width * count.max(0) as f32 / total))
            .bg(color)
    };
    v_flex()
        .id("detail-replicas")
        .aria_label(text.clone())
        .test_support()
        .gap_1()
        .child(div().text_sm().child(text))
        .child(
            h_flex()
                .h(px(8.))
                .w(px(width))
                .rounded_sm()
                .overflow_hidden()
                .bg(theme.muted)
                .child(segment(counts.available, theme.success))
                .child(segment(counts.updated - counts.available, theme.info))
                .child(segment(counts.unavailable, theme.warning)),
        )
        .into_any_element()
}

fn summary_table(table: &SummaryTable, cx: &App) -> AnyElement {
    summary_table_with_id(table, format!("detail-table-{}", table.title), cx)
}

fn summary_table_with_id(table: &SummaryTable, id: String, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let title = &table.title;
    // Cell ids are `<cell_prefix>-<col>`: the header's prefix and each body
    // row's differ, so every cell id is unique within the table (#89).
    let row = |id: SharedString, cell_prefix: &str, cells: &[String]| {
        h_flex()
            .id(id)
            .children(cells.iter().enumerate().map(|(index, text)| {
                table_cell(
                    SharedString::from(format!("{cell_prefix}-{index}")),
                    text.clone(),
                    if index == 0 { 200. } else { 160. },
                )
            }))
    };
    let head = format!("table-{title}-head");
    v_flex()
        .id(SharedString::from(id))
        .test_support()
        .pt_2()
        .text_sm()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(title.clone()),
        )
        .child(
            row(SharedString::from(head.clone()), &head, &table.columns)
                .text_color(theme.muted_foreground),
        )
        .children(table.rows.iter().enumerate().map(|(index, cells)| {
            row(
                SharedString::from(format!("table-{title}-row-{index}")),
                &format!("table-{title}-cell-{index}"),
                cells,
            )
        }))
        .into_any_element()
}

impl EventEmitter<OpenEditor> for ResourceDetailPanel {}
impl EventEmitter<OpenDetail> for ResourceDetailPanel {}
impl EventEmitter<OpenLogs> for ResourceDetailPanel {}
impl EventEmitter<OpenExec> for ResourceDetailPanel {}
impl EventEmitter<PanelEvent> for ResourceDetailPanel {}

impl Focusable for ResourceDetailPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ResourceDetailPanel {
    fn panel_name(&self) -> &'static str {
        "ResourceDetail"
    }
}

impl Panel for ResourceDetailPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.display_name().into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.display_name()
    }
}

impl Render for ResourceDetailPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = clock::now(cx);
        let (resolved, next_tick) = track_ages(now, || {
            detail::resolve(&self.target, self.session.read(cx), now)
        });
        self.tick = cx.spawn(async move |panel, cx| {
            cx.background_executor().timer(next_tick).await;
            panel.update(cx, |_, cx| cx.notify()).ok();
        });
        let Some(resolved) = resolved else {
            let text = format!("{} no longer exists", self.display_name());
            return v_flex()
                .track_focus(&self.focus_handle)
                .size_full()
                .child(
                    div()
                        .id("detail-gone")
                        .aria_label(text.clone())
                        .test_support()
                        .p_4()
                        .text_color(cx.theme().muted_foreground)
                        .child(text),
                )
                .into_any_element();
        };
        let model = resolved.model;
        let replicas = model.replicas;
        let forwards = forward_target(&model, &resolved.object.key);
        let mut metadata: Vec<AnyElement> = model
            .metadata
            .iter()
            .map(|(label, value)| field(label, value))
            .collect();
        metadata.extend(list_field("Labels", &model.labels));
        metadata.extend(list_field("Annotations", &model.annotations));
        metadata.extend(list_field("Owners", &model.owners));

        let mut status: Vec<AnyElement> = model
            .replicas
            .map(|counts| replica_bar(counts, cx))
            .into_iter()
            .collect();
        status.extend(
            model
                .status
                .iter()
                .map(|(label, value)| field(label, value)),
        );
        for condition in &model.conditions {
            let mut text = format!("{}={}", condition.kind, condition.status);
            if !condition.reason.is_empty() {
                text.push_str(&format!(" ({})", condition.reason));
            }
            if !condition.message.is_empty() {
                text.push_str(&format!(": {}", condition.message));
            }
            status.push(field("Condition", &text));
        }
        status.extend(model.tables.iter().map(|table| summary_table(table, cx)));
        status.extend(self.render_secret_keys(&model.secret_keys, cx));

        let yaml_lines = v_flex()
            .font_family("monospace")
            .text_xs()
            .children(model.yaml.lines().enumerate().map(|(index, line)| {
                div()
                    .id(("yaml-line", index))
                    .aria_label(line.to_string())
                    .test_support()
                    .whitespace_nowrap()
                    .child(if line.is_empty() {
                        " ".to_string()
                    } else {
                        line.to_string()
                    })
            }))
            .into_any_element();
        let metadata = section("detail-metadata", "Metadata", metadata, cx);
        let status = section("detail-status", "Status", status, cx);
        let sections = model
            .sections
            .iter()
            .map(|detail| {
                let children = detail
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| match item {
                        DetailItem::Field(field) => detail_field(field, cx),
                        DetailItem::Table(table) => summary_table_with_id(
                            table,
                            format!("detail-section-{}-table-{index}", detail.title),
                            cx,
                        ),
                    })
                    .collect();
                section(
                    format!("detail-section-{}", detail.title),
                    detail.title.clone(),
                    children,
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let related = resolved
            .related
            .map(|related| self.render_related(related, cx));
        let yaml = section("detail-yaml", "YAML", vec![yaml_lines], cx);
        let background = cx.theme().background;

        v_flex()
            .id("detail-tab")
            // The dock focuses the displayed panel; `cmd-w` dispatches from
            // here.
            .track_focus(&self.focus_handle)
            .aria_label(self.display_name())
            .test_support()
            .size_full()
            .child(self.render_toolbar(resolved.actions, model.suspended.unwrap_or(false), cx))
            .when_some(self.picking.clone(), |this, picking| {
                this.child(self.render_picker(picking, &model, cx))
            })
            .when_some(self.scale.clone(), |this, input| {
                this.child(self.render_scale(&input, replicas, cx))
            })
            .when(self.confirm_delete, |this| {
                this.child(self.render_confirm(resolved.cascade, cx))
            })
            .children(forwards.and_then(|target| self.render_forwards(target, cx)))
            .child(
                v_flex()
                    .id("detail-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .bg(background)
                    .child(metadata)
                    .child(status)
                    .children(sections)
                    .children(related)
                    .child(yaml),
            )
            .into_any_element()
    }
}
