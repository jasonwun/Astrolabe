//! What a view says about what it shows (#301): a [`Notice`] is an element
//! id, a text and an [`Intent`]. The views map the intent to a theme colour;
//! the layer holds no GPUI types.

use astrolabe_kube::cluster::{ClusterError, ClusterErrorKind};

/// How a notice's text is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    /// The default text colour (a listening Forward).
    Neutral,
    /// Loading, no permission, empty.
    Muted,
    /// A healthy connection.
    Success,
    /// Stale data, a connection on its way.
    Warning,
    /// A failure.
    Danger,
}

/// A text a view shows in place of, or beside, its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The element id the view gives the text; tests find it by this.
    pub id: &'static str,
    pub text: String,
    pub intent: Intent,
}

impl Notice {
    pub fn new(id: &'static str, text: impl Into<String>, intent: Intent) -> Self {
        Self {
            id,
            text: text.into(),
            intent,
        }
    }
}

/// The text shown where the watch of `noun` ("list", "object") failed before
/// its first snapshot. `error` is none when the watch ended without one.
pub fn failed_message(noun: &str, error: Option<&ClusterError>) -> String {
    match error {
        None => format!("The watch ended before the {noun} loaded"),
        Some(error) => match error.kind() {
            ClusterErrorKind::Connect => format!("Cannot connect to the cluster: {error}"),
            ClusterErrorKind::Unreachable => format!("Cannot reach the cluster: {error}"),
            _ => format!("Cannot load the {noun}: {error}"),
        },
    }
}
