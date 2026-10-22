//! Astrolabe desktop application: store and session entities live here (per
//! the workspace decision, they are main-thread-resident GPUI entities — no
//! trait boundary). `astrolabe-kube` (cluster I/O) and `astrolabe-terminal`
//! (terminal engine + view) plug in here.

pub mod kube_bridge;
pub mod yaml_editor;
