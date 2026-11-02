//! The model layer (ADR-0002): Session's state and rules, synchronous and
//! free of I/O. It never reads the clock; callers pass the time in.
