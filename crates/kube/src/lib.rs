//! Kubernetes communication layer (kube-rs): client construction, exec
//! credential plugins, watchers, server-side apply.
//!
//! This crate MUST NOT depend on gpui — it is pure tokio-side code so the
//! GPUI main thread never touches cluster I/O. Guarded by code review and
//! by `cargo tree` (no gpui in this crate's dependency closure).

pub use k8s_openapi;

mod client;

pub use client::default_client;
