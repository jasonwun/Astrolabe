//! Astrolabe desktop application: store and session entities live here (per
//! the workspace decision, they are main-thread-resident GPUI entities — no
//! trait boundary). `astrolabe-kube` (cluster I/O) and `astrolabe-terminal`
//! (terminal engine + view) plug in here.

pub mod builtin;
pub mod clock;
mod cluster_io;
pub mod exec;
pub mod jsonpath;
pub mod keymap;
pub mod kube_bridge;
pub mod logs;
pub mod projection;
mod resource_detail;
mod resource_list;
pub mod session;
mod session_view;
pub mod store;
pub mod workspace;
pub mod yaml_editor;

/// The GPUI application with the bundled icon SVGs registered. Without
/// them every icon (e.g. a tab's close button) draws nothing while its
/// button still takes clicks (#51).
#[allow(clippy::disallowed_methods)]
pub fn application() -> gpui_kit::Application {
    gpui_kit::application().with_assets(gpui_kit::assets::Assets)
}
