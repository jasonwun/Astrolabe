//! The status line of the Logs and Exec tabs (#304): what each stream or
//! session says about itself, as a [`Notice`].

use astrolabe_kube::cluster::ClusterError;

use crate::notice::{Intent, Notice};

const LOGS_STATUS: &str = "logs-status";
const EXEC_STATUS: &str = "exec-status";

/// Where a followed container log stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogState {
    Streaming,
    Ended,
    Failed(ClusterError),
}

impl LogState {
    /// The stream has ended or failed: reopening starts a new one.
    pub fn is_finished(&self) -> bool {
        !matches!(self, Self::Streaming)
    }

    pub fn notice(&self) -> Notice {
        match self {
            Self::Streaming => Notice::new(LOGS_STATUS, "Streaming…", Intent::Muted),
            Self::Ended => Notice::new(LOGS_STATUS, "Stream ended", Intent::Muted),
            Self::Failed(error) => Notice::new(
                LOGS_STATUS,
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
    /// The session ended without an exit status.
    pub fn dropped() -> Self {
        Self::Failed(ClusterError::new("session dropped"))
    }

    /// The shell has exited or failed: reopening starts a new one.
    pub fn is_finished(&self) -> bool {
        !matches!(self, Self::Running)
    }

    pub fn notice(&self) -> Notice {
        match self {
            Self::Running => Notice::new(EXEC_STATUS, "Session running", Intent::Muted),
            Self::Exited(code) => Notice::new(
                EXEC_STATUS,
                format!("Exited with code {code}"),
                Intent::Muted,
            ),
            Self::Failed(error) => Notice::new(
                EXEC_STATUS,
                format!("Session failed: {error}"),
                Intent::Danger,
            ),
        }
    }
}
