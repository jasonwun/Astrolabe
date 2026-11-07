//! The detail panel's action flow (#300): the four small state machines a
//! detail tab runs when a toolbar action is pressed (the delete
//! confirmation, the container/shell/port picker, the Scale form and the
//! notice), with no view. Each step answers with an [`Effect`] the view
//! carries out.

use std::fmt::Display;

use astrolabe_kube::object::{ObjectKey, ObjectRef, Operation};
use astrolabe_kube::pod::{ExecRequest, ForwardRequest, ForwardTarget, LogRequest};

use crate::builtin::Action;
use crate::detail::{DetailTarget, Resolved};
use crate::operation_text;
use crate::projection::{Containers, DetailModel, ForwardTo};
use crate::session::ForwardState;
use crate::session_state::SessionState;

/// The shells Exec offers; `sh` exists in nearly every image.
pub const SHELLS: &[&str] = &["sh", "bash"];

/// A Pod action that runs in one container; Forward also serves Services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PodAction {
    Logs,
    Exec,
    /// Picks a port, not a container.
    Forward,
}

/// What the picker asks: a container, or for Exec in a chosen container,
/// a shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picking {
    pub action: PodAction,
    pub container: Option<String>,
}

impl Picking {
    /// The question the picker shows.
    pub fn prompt(&self) -> String {
        match (self.action, &self.container) {
            (PodAction::Logs, _) => "Logs: choose a container".to_string(),
            (PodAction::Exec, None) => "Exec: choose a container".to_string(),
            (PodAction::Exec, Some(container)) => {
                format!("Exec in {container}: choose a shell")
            }
            (PodAction::Forward, _) => "Port forward: choose a port".to_string(),
        }
    }
}

/// What the view does after a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Open a Logs tab following this container.
    OpenLogs {
        title: String,
        request: LogRequest,
    },
    /// Open an Exec tab running a shell.
    OpenExec {
        title: String,
        request: ExecRequest,
    },
    /// Forward a port.
    Forward(ForwardRequest),
    /// Send the operation, then report it back with
    /// [`DetailActions::finished`].
    Run(Operation),
}

/// What the shown object offers an action: read out of the resolved detail
/// once per step.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Subject {
    /// The Pod's key, when the target is a built-in object.
    pub pod: Option<ObjectKey>,
    pub containers: Containers,
    /// What Port Forward reaches, when the kind offers it.
    pub forward: Option<ForwardTarget>,
    pub suspended: bool,
    /// The object in its watch; `None` once it vanished.
    pub object: Option<ObjectRef>,
}

impl Subject {
    pub fn of(target: &DetailTarget, state: &SessionState, resolved: Option<&Resolved>) -> Self {
        let pod = match target {
            DetailTarget::Builtin { key, .. } => Some(key.clone()),
            _ => None,
        };
        Self {
            pod,
            containers: resolved
                .and_then(|resolved| resolved.model.containers.clone())
                .unwrap_or_default(),
            forward: resolved
                .and_then(|resolved| forward_target(&resolved.model, &resolved.object.key)),
            suspended: resolved.is_some_and(|resolved| resolved.model.is_suspended()),
            object: target.object(state),
        }
    }

    fn containers_for(&self, action: PodAction) -> &[String] {
        match action {
            PodAction::Logs => &self.containers.all,
            PodAction::Exec | PodAction::Forward => &self.containers.running,
        }
    }
}

/// The containers `action` can target: Logs reads every container, init
/// containers first; Exec and Forward need one still running.
pub fn containers(model: &DetailModel, action: PodAction) -> Vec<String> {
    let Some(containers) = &model.containers else {
        return Vec::new();
    };
    match action {
        PodAction::Logs => containers.all.clone(),
        PodAction::Exec | PodAction::Forward => containers.running.clone(),
    }
}

/// What Port Forward reaches from the detail of the object at `key`.
pub fn forward_target(model: &DetailModel, key: &ObjectKey) -> Option<ForwardTarget> {
    Some(match model.forward.as_ref()?.to {
        ForwardTo::Pod => ForwardTarget::Pod(key.clone()),
        ForwardTo::Service => ForwardTarget::Service(key.clone()),
    })
}

/// The state of a detail tab's actions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailActions {
    confirm_delete: bool,
    scale_open: bool,
    picking: Option<Picking>,
    notice: Option<String>,
}

impl DetailActions {
    pub fn confirming_delete(&self) -> bool {
        self.confirm_delete
    }

    pub fn scale_open(&self) -> bool {
        self.scale_open
    }

    pub fn picking(&self) -> Option<&Picking> {
        self.picking.as_ref()
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// A toolbar action was pressed. Edit YAML opens no flow here: the view
    /// handles it.
    pub fn press(&mut self, action: Action, subject: &Subject) -> Effect {
        match action {
            Action::Logs => self.pick_container(PodAction::Logs, subject),
            Action::Exec => self.pick_container(PodAction::Exec, subject),
            Action::PortForward => {
                self.ask(PodAction::Forward, None);
                Effect::None
            }
            Action::Scale => {
                self.scale_open = true;
                Effect::None
            }
            Action::RolloutRestart => {
                self.run(subject, |object| Operation::RolloutRestart { object })
            }
            Action::Suspend => {
                let suspend = !subject.suspended;
                self.run(subject, |object| Operation::SetSuspend { object, suspend })
            }
            Action::Trigger => self.run(subject, |object| Operation::TriggerCronJob { object }),
            Action::Edit => Effect::None,
            Action::Delete => {
                self.confirm_delete = true;
                Effect::None
            }
        }
    }

    fn pick_container(&mut self, action: PodAction, subject: &Subject) -> Effect {
        match subject.containers_for(action) {
            [only] => self.choose_container(action, only.clone(), subject),
            _ => {
                self.ask(action, None);
                Effect::None
            }
        }
    }

    fn ask(&mut self, action: PodAction, container: Option<String>) {
        self.picking = Some(Picking { action, container });
    }

    /// A container was chosen (or was the only one) for `action`.
    pub fn choose_container(
        &mut self,
        action: PodAction,
        container: String,
        subject: &Subject,
    ) -> Effect {
        let Some(key) = &subject.pod else {
            return Effect::None;
        };
        match action {
            PodAction::Logs => {
                self.picking = None;
                Effect::OpenLogs {
                    title: format!("Logs {key} ({container})"),
                    request: LogRequest {
                        pod: key.clone(),
                        container,
                    },
                }
            }
            PodAction::Exec => {
                self.ask(action, Some(container));
                Effect::None
            }
            PodAction::Forward => Effect::None,
        }
    }

    /// A shell was chosen for Exec in `container`.
    pub fn choose_shell(&mut self, container: String, shell: &str, subject: &Subject) -> Effect {
        self.picking = None;
        let Some(key) = &subject.pod else {
            return Effect::None;
        };
        Effect::OpenExec {
            title: format!("Exec {key} ({container})"),
            request: ExecRequest {
                pod: key.clone(),
                container,
                shell: shell.to_string(),
            },
        }
    }

    /// A port was chosen to forward.
    pub fn choose_port(&mut self, port: u16, subject: &Subject) -> Effect {
        self.picking = None;
        match &subject.forward {
            Some(target) => Effect::Forward(ForwardRequest {
                target: target.clone(),
                port,
            }),
            None => Effect::None,
        }
    }

    pub fn cancel_picker(&mut self) {
        self.picking = None;
    }

    /// The Scale field's text was submitted. A bad number only sets the
    /// notice; the form closes once the operation is sent.
    pub fn submit_scale(&mut self, text: &str, subject: &Subject) -> Effect {
        let replicas = match text.trim().parse::<i32>() {
            Ok(replicas) if replicas >= 0 => replicas,
            _ => {
                self.notice = Some("Replicas must be a whole number, 0 or more".to_string());
                return Effect::None;
            }
        };
        let effect = self.run(subject, |object| Operation::Scale { object, replicas });
        if matches!(effect, Effect::Run(_)) {
            self.scale_open = false;
        }
        effect
    }

    pub fn cancel_scale(&mut self) {
        self.scale_open = false;
    }

    /// Delete was confirmed.
    pub fn confirm_delete(&mut self, subject: &Subject) -> Effect {
        self.confirm_delete = false;
        self.run(subject, |object| Operation::Delete { object })
    }

    pub fn cancel_delete(&mut self) {
        self.confirm_delete = false;
    }

    /// Send the operation `operation` builds for the shown object, showing
    /// its pending text until [`finished`](Self::finished). Sends nothing
    /// once the object is gone.
    fn run(&mut self, subject: &Subject, operation: impl FnOnce(ObjectRef) -> Operation) -> Effect {
        let Some(object) = subject.object.clone() else {
            return Effect::None;
        };
        let operation = operation(object);
        self.notice = Some(operation_text::pending(&operation).to_string());
        Effect::Run(operation)
    }

    /// The server answered `operation`.
    pub fn finished<E: Display>(&mut self, operation: &Operation, result: Result<(), E>) {
        self.notice = Some(match result {
            Ok(()) => operation_text::done(operation),
            Err(error) => format!("{} failed: {error}", operation_text::verb(operation)),
        });
    }
}

/// How a Forward row's text is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardTone {
    Muted,
    Normal,
    Danger,
}

/// A Forward row's status text and tone.
pub fn forward_row(state: &ForwardState, port: u16) -> (String, ForwardTone) {
    match state {
        ForwardState::Starting => (format!("Starting → {port}…"), ForwardTone::Muted),
        ForwardState::Listening(local) => {
            (format!("localhost:{local} → {port}"), ForwardTone::Normal)
        }
        ForwardState::Failed(error) => (
            format!("Forward to {port} failed: {error}"),
            ForwardTone::Danger,
        ),
    }
}
