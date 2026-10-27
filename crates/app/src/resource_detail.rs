//! Generic detail tab (#11): metadata summary, status summary, read-only
//! YAML; toolbar Edit YAML (separate editor tab, SSA) and Delete. It reads
//! the object out of the list watch's store, so it refreshes with the watch
//! and issues no GET/watch of its own. Built-in kinds reuse this template
//! through their registration (#17).

use astrolabe_kube::crd::KindResource;
use std::collections::BTreeSet;

use astrolabe_kube::k8s_openapi::api::batch::v1::CronJob;
use astrolabe_kube::k8s_openapi::api::core::v1::{Namespace, Pod, Secret, Service};
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::k8s_openapi::serde::de::DeserializeOwned;
use astrolabe_kube::kube::Resource;
use astrolabe_kube::kube::discovery::ApiResource;
use gpui_kit::base::test_support::Observed;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tokio::sync::oneshot;

use crate::builtin::{Action, BuiltinKind, DEFAULT_ACTIONS};
use crate::projection::config::{HIDDEN, SecretKey, secret_value};
use crate::projection::network::service_ports;
use crate::projection::workloads::{pod_containers, pod_ports, running_containers};
use crate::projection::{
    DetailField, DetailItem, DetailModel, ReplicaCounts, ResourceTable, SummaryTable,
    custom_resource_detail, definition_detail, namespace_detail,
};
use crate::resource_list::OpenDetail;
use crate::session::{
    CustomKindId, ExecRequest, ForwardRequest, ForwardState, ForwardTarget, LogRequest, ObjectRef,
    Operation, Session,
};
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
    focus_handle: FocusHandle,
}

/// A Pod action that runs in one container; Forward also serves Services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PodAction {
    Logs,
    Exec,
    /// Picks a port, not a container.
    Forward,
}

/// What the picker asks: a container, or for Exec in a chosen container,
/// a shell.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Picking {
    action: PodAction,
    container: Option<String>,
}

/// The shells Exec offers; `sh` exists in nearly every image.
const SHELLS: &[&str] = &["sh", "bash"];

/// The object as of this render, with what its actions need.
struct Resolved {
    model: DetailModel,
    object: ObjectRef,
    cascade: String,
    actions: &'static [Action],
    related: Option<RelatedRows>,
}

/// A related section as of this render.
struct RelatedRows {
    title: &'static str,
    kind: BuiltinKind,
    table: ResourceTable,
    forbidden: bool,
}

impl ResourceDetailPanel {
    pub fn new(session: Entity<Session>, target: DetailTarget, cx: &mut Context<Self>) -> Self {
        if let DetailTarget::Builtin { kind, key } = &target
            && let Some(namespace) = &key.namespace
        {
            session.update(cx, |session, cx| {
                session.watch_builtin(*kind, namespace, cx);
                if let Some(related) = &kind.registration().related {
                    session.watch_builtin(related.kind, namespace, cx);
                }
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
            scale: None,
            picking: None,
            revealed: BTreeSet::new(),
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
                    related: None,
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
                    related: None,
                })
            }
            DetailTarget::Builtin { kind, key } => {
                let registration = kind.registration();
                let namespace = key.namespace.as_deref()?;
                let watch = session.builtin_watch_key(*kind, namespace);
                let object = session
                    .object_watch(&watch)?
                    .get(Some(namespace), &key.name)?;
                let related = registration.related.as_ref().and_then(|related| {
                    let watch =
                        session.object_watch(&session.builtin_watch_key(related.kind, namespace));
                    let candidates = watch.map(|watch| watch.objects()).unwrap_or_default();
                    let selected = (related.select)(object, &candidates);
                    let forbidden = watch.is_some_and(|watch| watch.forbidden());
                    if related.hide_when_empty && selected.is_empty() && !forbidden {
                        return None;
                    }
                    Some(RelatedRows {
                        title: related.title,
                        kind: related.kind,
                        table: (related.kind.registration().list)(&selected, now),
                        forbidden,
                    })
                });
                Some(Resolved {
                    model: (registration.detail)(object, now)?,
                    object: ObjectRef {
                        resource: (registration.resource)(),
                        key: key.clone(),
                    },
                    cascade: (registration.cascade)(object).to_string(),
                    actions: registration.actions,
                    related,
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
                    related: None,
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

    fn open_scale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn rollout_restart(&mut self, cx: &mut Context<Self>) {
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

    fn set_suspend(&mut self, suspend: bool, cx: &mut Context<Self>) {
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

    fn trigger(&mut self, cx: &mut Context<Self>) {
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

    /// The shown Pod's containers `action` can target: Logs reads every
    /// container, init containers first; Exec needs one still running.
    fn containers(&self, action: PodAction, cx: &App) -> Vec<String> {
        let Some(pod) = self.pod(cx) else {
            return Vec::new();
        };
        match action {
            PodAction::Logs => pod_containers(&pod),
            PodAction::Exec | PodAction::Forward => running_containers(&pod),
        }
    }

    /// The shown Pod, as of the watch.
    fn pod(&self, cx: &App) -> Option<Pod> {
        self.typed(BuiltinKind::Pods, cx)
    }

    /// The shown object as `K`, when it is of built-in `kind`.
    fn typed<K: Resource + DeserializeOwned>(&self, kind: BuiltinKind, cx: &App) -> Option<K> {
        let DetailTarget::Builtin { kind: shown, key } = &self.target else {
            return None;
        };
        if *shown != kind {
            return None;
        }
        let namespace = key.namespace.as_deref()?;
        let session = self.session.read(cx);
        session
            .object_watch(&session.builtin_watch_key(kind, namespace))
            .and_then(|watch| watch.get(Some(namespace), &key.name))
            .and_then(|object| object.clone().try_parse::<K>().ok())
    }

    /// What Port Forward reaches from this tab: the Pod or the Service.
    fn forward_target(&self) -> Option<ForwardTarget> {
        match &self.target {
            DetailTarget::Builtin {
                kind: BuiltinKind::Pods,
                key,
            } => Some(ForwardTarget::Pod(key.clone())),
            DetailTarget::Builtin {
                kind: BuiltinKind::Services,
                key,
            } => Some(ForwardTarget::Service(key.clone())),
            _ => None,
        }
    }

    /// Run `action` in the only container, or ask which one.
    fn pick_container(&mut self, action: PodAction, cx: &mut Context<Self>) {
        match self.containers(action, cx).as_slice() {
            [only] => self.run_in(action, only.clone(), cx),
            _ => self.ask(action, None, cx),
        }
    }

    fn ask(&mut self, action: PodAction, container: Option<String>, cx: &mut Context<Self>) {
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
        let Some(target) = self.forward_target() else {
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

    fn render_picker(&self, picking: Picking, cx: &mut Context<Self>) -> impl IntoElement {
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
                (PodAction::Forward, _) => self.render_ports(cx),
                (_, None) => self
                    .containers(action, cx)
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
    fn render_ports(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let border = cx.theme().border;
        let ports = match self.forward_target() {
            Some(ForwardTarget::Pod(_)) => self.pod(cx).map(|pod| pod_ports(&pod)),
            Some(ForwardTarget::Service(_)) => self
                .typed::<Service>(BuiltinKind::Services, cx)
                .map(|service| service_ports(&service)),
            None => None,
        }
        .unwrap_or_default();
        if ports.is_empty() {
            let text = match self.forward_target() {
                Some(ForwardTarget::Service(_)) => "No TCP ports",
                _ => "No declared ports",
            };
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
    fn render_forwards(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = self.forward_target()?;
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

    /// A Secret's keys, each value masked until revealed; a revealed value
    /// is decoded from the watch at render time.
    fn render_secret_keys(&self, keys: &[SecretKey], cx: &mut Context<Self>) -> Option<AnyElement> {
        if keys.is_empty() {
            return None;
        }
        let border = cx.theme().border;
        let secret = self.typed::<Secret>(BuiltinKind::Secrets, cx);
        let rows = keys.iter().map(|entry| {
            let key = entry.key.clone();
            let revealed = self.revealed.contains(&key);
            let value = secret
                .as_ref()
                .filter(|_| revealed)
                .and_then(|secret| secret_value(secret, &key))
                .unwrap_or_else(|| format!("{HIDDEN} ({} bytes)", entry.bytes));
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

    fn render_toolbar(&self, actions: &[Action], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let button = |id: &'static str, label: &'static str| button(id, label, border);
        let suspended = self
            .typed::<CronJob>(BuiltinKind::CronJobs, cx)
            .and_then(|cronjob| cronjob.spec.suspend)
            .unwrap_or(false);
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

    fn render_scale(
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

    /// The related kind's list columns over the selected objects; a row's
    /// first cell opens its detail.
    fn render_related(&self, related: RelatedRows, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let cell = |id: SharedString, text: String, first: bool| {
            div()
                .id(id)
                .aria_label(text.clone())
                .test_support()
                .w(px(if first { 240. } else { 140. }))
                .flex_shrink_0()
                .pr_2()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(text)
        };
        let lowercase = related.title.to_lowercase();
        let empty = if related.forbidden {
            Some(format!("No permission to list {lowercase} here"))
        } else if related.table.rows.is_empty() {
            Some(format!("No {lowercase}"))
        } else {
            None
        };
        let mut children = vec![
            div()
                .id("related-title")
                .aria_label(related.title)
                .test_support()
                .text_xs()
                .text_color(muted)
                .child(related.title)
                .into_any_element(),
        ];
        match empty {
            Some(text) => children.push(
                div()
                    .id("related-empty")
                    .aria_label(text.clone())
                    .test_support()
                    .text_sm()
                    .text_color(muted)
                    .child(text)
                    .into_any_element(),
            ),
            None => {
                children.push(
                    h_flex()
                        .id("related-head")
                        .text_sm()
                        .text_color(muted)
                        .children(
                            related
                                .table
                                .columns
                                .iter()
                                .enumerate()
                                .map(|(index, name)| {
                                    cell(
                                        SharedString::from(format!("related-column-{index}")),
                                        name.clone(),
                                        index == 0,
                                    )
                                }),
                        )
                        .into_any_element(),
                );
                for (row_index, row) in related.table.rows.into_iter().enumerate() {
                    let target = DetailTarget::Builtin {
                        kind: related.kind,
                        key: row.key.clone(),
                    };
                    let mut cells = row.cells.into_iter().enumerate().map(|(index, text)| {
                        cell(
                            SharedString::from(format!("related-cell-{row_index}-{index}")),
                            text,
                            index == 0,
                        )
                    });
                    let first = cells.next().map(|first| {
                        first
                            .id(SharedString::from(format!("related-row-{}", row.key)))
                            .cursor_pointer()
                            .text_color(theme.link)
                            .on_click(
                                cx.listener(move |_, _, _, cx| cx.emit(OpenDetail(target.clone()))),
                            )
                    });
                    children.push(
                        h_flex()
                            .text_sm()
                            .children(first)
                            .children(cells)
                            .into_any_element(),
                    );
                }
            }
        }
        v_flex()
            .id("detail-related")
            .test_support()
            .gap_1()
            .p_2()
            .children(children)
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
        let Some(resolved) = self.resolve(cx) else {
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
            // The dock focuses the displayed panel; `cmd-w` dispatches from here.
            .track_focus(&self.focus_handle)
            .aria_label(self.display_name())
            .test_support()
            .size_full()
            .child(self.render_toolbar(resolved.actions, cx))
            .when_some(self.picking.clone(), |this, picking| {
                this.child(self.render_picker(picking, cx))
            })
            .when_some(self.scale.clone(), |this, input| {
                this.child(self.render_scale(&input, replicas, cx))
            })
            .when(self.confirm_delete, |this| {
                this.child(self.render_confirm(resolved.cascade, cx))
            })
            .children(self.render_forwards(cx))
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
