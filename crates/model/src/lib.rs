//! The model layer (ADR-0002): Session's state and rules, synchronous and
//! free of I/O. It never reads the clock; callers pass the time in.

pub mod builtin;
pub mod catalog;
pub mod detail;
pub mod detail_actions;
pub mod discovery_cache;
pub mod edit_loop;
pub mod jsonpath;
pub mod list;
pub mod notice;
pub mod operation_text;
pub mod projection;
pub mod session;
pub mod session_state;
