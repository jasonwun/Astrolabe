//! What a panel asks the session view for (#303): one request for every
//! kind of tab, and the one rule for which requests reuse a tab.

use astrolabe_kube::pod::{ExecRequest, LogRequest};

use crate::detail::DetailTarget;

/// A tab a panel asks for. The session view builds the panel; this says
/// what it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabRequest {
    /// The detail of one object.
    Detail(DetailTarget),
    /// A Logs tab following a container.
    Logs { title: String, request: LogRequest },
    /// An Exec tab running a shell.
    Exec { title: String, request: ExecRequest },
    /// An editor tab on this text.
    Editor { title: String, yaml: String },
}

/// What identifies a tab that a later request may reuse.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TabKey {
    Detail(DetailTarget),
}

impl TabRequest {
    /// The key of the tab this request reuses: `Some` shows the open tab
    /// with this key if there is one, `None` always opens a new tab. Only a
    /// detail is reused.
    pub fn key(&self) -> Option<TabKey> {
        match self {
            Self::Detail(target) => Some(TabKey::Detail(target.clone())),
            Self::Logs { .. } | Self::Exec { .. } | Self::Editor { .. } => None,
        }
    }
}
