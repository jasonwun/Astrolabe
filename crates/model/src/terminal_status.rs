//! The status line of the Logs and Exec tabs (#304): what each stream or
//! session says about itself, as a [`Notice`].

use astrolabe_kube::cluster::ClusterError;

use crate::notice::{Intent, Notice};

/// Where a followed container log stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogState {
    Streaming,
    Ended,
    Failed(ClusterError),
}

impl LogState {
    pub fn notice(&self) -> Notice {
        match self {
            Self::Streaming => Notice::new("logs-status", "Streaming…", Intent::Muted),
            Self::Ended => Notice::new("logs-status", "Stream ended", Intent::Muted),
            Self::Failed(error) => Notice::new(
                "logs-status",
                format!("Stream failed: {error}"),
                Intent::Danger,
            ),
        }
    }
}

/// Where an exec session stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecState {
    Running,
    Exited(u8),
    Failed(ClusterError),
}

impl ExecState {
    pub fn notice(&self) -> Notice {
        match self {
            Self::Running => Notice::new("exec-status", "Session running", Intent::Muted),
            Self::Exited(code) => Notice::new(
                "exec-status",
                format!("Exited with code {code}"),
                Intent::Muted,
            ),
            Self::Failed(error) => Notice::new(
                "exec-status",
                format!("Session failed: {error}"),
                Intent::Danger,
            ),
        }
    }
}
