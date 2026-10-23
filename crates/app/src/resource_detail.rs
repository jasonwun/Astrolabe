//! Generic detail tab (#11): metadata summary, status summary, read-only
//! YAML; toolbar Edit YAML (separate editor tab, SSA) and Delete. It reads
//! the object out of the list watch's store, so it refreshes with the watch
//! and issues no GET/watch of its own. Built-in kinds reuse this template
//! through their registration (#17).

use astrolabe_kube::crd::KindResource;
use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::discovery::ApiResource;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::builtin::{Action, BuiltinKind, DEFAULT_ACTIONS};
use crate::projection::{
    DetailModel, ReplicaCounts, SummaryTable, custom_resource_detail, definition_detail,
    namespace_detail,
};
use crate::session::{CustomKindId, ObjectRef, Session};
use crate::store::ObjectKey;
use crate::yaml_editor::YamlEditorView;

/// What a detail tab shows: one object, addressed in its list's store.
/// Custom resources are version-free (#41): each resolve reads the watch at
/// the kind's current preferred version, so a version change keeps the tab.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DetailTarget {
    Namespace { name: String },
    Definition { name: String },
    Builtin { kind: BuiltinKind, key: ObjectKey },
    Custom { id: CustomKindId, key: ObjectKey },
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
    notice: Option<String>,
    focus_handle: FocusHandle,
}

/// The object as of this render, with what its actions need.
struct Resolved {
    model: DetailModel,
    object: ObjectRef,
    cascade: String,
    actions: &'static [Action],
}

impl ResourceDetailPanel {
    pub fn new(session: Entity<Session>, target: DetailTarget, cx: &mut Context<Self>) -> Self {
        if let DetailTarget::Builtin { kind, key } = &target
            && let Some(namespace) = &key.namespace
        {
            session.update(cx, |session, cx| {
                session.watch_builtin(*kind, namespace, cx)
            });
        }
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
        Self {
            session,
            target,
            confirm_delete: false,
            notice: None,
            focus_handle: cx.focus_handle(),
        }
    }

    fn resolve(&self, cx: &App) -> Option<Resolved> {
        let session = self.session.read(cx);
        let now = Timestamp::now();
        match &self.target {
            DetailTarget::Namespace { name } => {
                let namespace = session.namespace_object(name)?;
                Some(Resolved {
                    model: namespace_detail(namespace, now),
                    object: ObjectRef {
                        resource: ApiResource::erase::<Namespace>(&()),
                        key: ObjectKey::cluster_scoped(name.clone()),
                    },
                    cascade: "Deleting a Namespace deletes every object in it.".to_string(),
                    actions: DEFAULT_ACTIONS,
                })
            }
            DetailTarget::Definition { name } => {
                let crd = session.crds().get(None, name)?;
                Some(Resolved {
                    model: definition_detail(crd, now),
                    object: ObjectRef {
                        resource: ApiResource::erase::<CustomResourceDefinition>(&()),
                        key: ObjectKey::cluster_scoped(name.clone()),
                    },
                    cascade: format!(
                        "Deleting a CustomResourceDefinition also deletes every {} object in the cluster.",
                        crd.spec.names.kind
                    ),
                    actions: DEFAULT_ACTIONS,
                })
            }
            DetailTarget::Builtin { kind, key } => {
                let registration = kind.registration();
                let namespace = key.namespace.as_deref()?;
                let watch = session.builtin_watch_key(*kind, namespace);
                let object = session
                    .object_watch(&watch)?
                    .get(Some(namespace), &key.name)?;
                Some(Resolved {
                    model: (registration.detail)(object, now)?,
                    object: ObjectRef {
                        resource: (registration.resource)(),
                        key: key.clone(),
                    },
                    cascade: registration.cascade.to_string(),
                    actions: registration.actions,
                })
            }
            DetailTarget::Custom { id, key } => {
                let kind = session.custom_kind(id)?;
                let KindResource::Served(resource) = &kind.resource else {
                    return None;
                };
                let namespace = key.namespace.as_deref();
                let watch = session.custom_object_watch_key(kind, namespace);
                let object = session.object_watch(&watch)?.get(namespace, &key.name)?;
                Some(Resolved {
                    model: custom_resource_detail(kind, object, now),
                    object: ObjectRef {
                        resource: resource.clone(),
                        key: key.clone(),
                    },
                    cascade: "Objects that list it in ownerReferences will be garbage-collected."
                        .to_string(),
                    actions: DEFAULT_ACTIONS,
                })
            }
        }
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
                yaml: resolved.model.yaml,
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
        self.notice = Some("Deleting…".to_string());
        cx.notify();
        cx.spawn(async move |panel, cx| {
            let result = result
                .await
                .unwrap_or_else(|_| Err("delete task dropped".to_string()));
            panel
                .update(cx, |panel, cx| {
                    panel.notice = Some(match result {
                        Ok(()) => "Deletion requested".to_string(),
                        Err(error) => format!("Delete failed: {error}"),
                    });
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn render_toolbar(&self, actions: &[Action], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .aria_label(label)
                .test_support()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .text_sm()
                .child(label)
        };
        h_flex()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(theme.border)
            .children(actions.iter().map(|action| {
                match action {
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

fn section(
    id: &'static str,
    title: &'static str,
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
                .child(title),
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
    let theme = cx.theme();
    let title = &table.title;
    let row = |id: SharedString, cells: &[String]| {
        h_flex()
            .id(id)
            .children(cells.iter().enumerate().map(|(index, cell)| {
                div()
                    .id(SharedString::from(format!("table-{title}-cell-{index}")))
                    .aria_label(cell.clone())
                    .test_support()
                    .w(px(if index == 0 { 200. } else { 160. }))
                    .flex_shrink_0()
                    .pr_2()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(cell.clone())
            }))
    };
    v_flex()
        .id(SharedString::from(format!("detail-table-{title}")))
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
            row(
                SharedString::from(format!("table-{title}-head")),
                &table.columns,
            )
            .text_color(theme.muted_foreground),
        )
        .children(table.rows.iter().enumerate().map(|(index, cells)| {
            row(
                SharedString::from(format!("table-{title}-row-{index}")),
                cells,
            )
        }))
        .into_any_element()
}

impl EventEmitter<OpenEditor> for ResourceDetailPanel {}
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
        let Some(resolved) = self.resolve(cx) else {
            let text = format!("{} no longer exists", self.display_name());
            return v_flex()
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
        let yaml = section("detail-yaml", "YAML", vec![yaml_lines], cx);
        let background = cx.theme().background;

        v_flex()
            .id("detail-tab")
            .aria_label(self.display_name())
            .test_support()
            .size_full()
            .child(self.render_toolbar(resolved.actions, cx))
            .when(self.confirm_delete, |this| {
                this.child(self.render_confirm(resolved.cascade, cx))
            })
            .child(
                v_flex()
                    .id("detail-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .bg(background)
                    .child(metadata)
                    .child(status)
                    .child(yaml),
            )
            .into_any_element()
    }
}

/// The Edit YAML tab: the #21 editor loop on one object's manifest.
pub struct EditorPanel {
    title: String,
    editor: Entity<YamlEditorView>,
}

impl EditorPanel {
    pub fn new(title: String, editor: Entity<YamlEditorView>) -> Self {
        Self { title, editor }
    }
}

impl EventEmitter<PanelEvent> for EditorPanel {}

impl Focusable for EditorPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl BasePanel for EditorPanel {
    fn panel_name(&self) -> &'static str {
        "YamlEditor"
    }
}

impl Panel for EditorPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(format!("Edit {}", self.title).into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        format!("Edit {}", self.title)
    }
}

impl Render for EditorPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("yaml-edit-tab")
            .aria_label(format!("Edit {}", self.title))
            .test_support()
            .size_full()
            .child(self.editor.clone())
    }
}
