//! Astrolabe desktop application: store and session entities live here (per
//! the workspace decision, they are main-thread-resident GPUI entities — no
//! trait boundary). `astrolabe-kube` (cluster I/O) and `astrolabe-terminal`
//! (terminal engine + view) plug in here.

pub mod builtin;
mod cluster_io;
pub mod exec;
pub mod jsonpath;
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
