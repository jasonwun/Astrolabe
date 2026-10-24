//! Terminal: engine boundary (`TerminalBackend` trait), the
//! `alacritty_terminal` implementation, and the GPUI view.
//!
//! Engine (alacritty/vte) types never appear in public signatures outside
//! `backend::alacritty`; the view and the app only see the types defined
//! in [`backend`]. The display-only (no PTY) path doubles as the
//! log-stream terminal (`kubectl logs -f`); the remote path renders the
//! same way and sends input to a far end (`kubectl exec`).

pub mod backend;
pub mod view;

pub use backend::TerminalBackend;
pub use view::TerminalView;
