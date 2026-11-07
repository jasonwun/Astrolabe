//! The event a panel emits to ask for a tab. It lives apart from
//! `SessionView` so the panels do not import the view that hosts them.

use astrolabe_model::tab::TabRequest;

/// A panel asks for a tab.
pub struct OpenTab(pub TabRequest);
