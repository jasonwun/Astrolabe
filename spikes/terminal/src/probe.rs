//! #92 instrumentation: where does `feed_output` time go?
//!
//! Splits every feed into `Term` lock wait and lock hold, records the
//! thread's CPU time over the hold, and samples which core it ran on
//! (Apple Silicon: TPIDR_EL0 holds the CPU id; ids < 0x100 are the
//! efficiency cluster on M3 Max).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

pub static FEED_WAIT_NS: AtomicU64 = AtomicU64::new(0);
pub static FEED_HOLD_NS: AtomicU64 = AtomicU64::new(0);
pub static FEED_HOLD_CPU_NS: AtomicU64 = AtomicU64::new(0);
pub static FEED_ON_E: AtomicU64 = AtomicU64::new(0);
pub static FEED_ON_P: AtomicU64 = AtomicU64::new(0);
pub static FEED_CONTENDED: AtomicU64 = AtomicU64::new(0);
pub static SNAP_WAIT_NS: AtomicU64 = AtomicU64::new(0);
pub static SNAP_HOLD_NS: AtomicU64 = AtomicU64::new(0);
pub static SNAP_N: AtomicU64 = AtomicU64::new(0);
pub static CALIB_NS: AtomicU64 = AtomicU64::new(0);
pub static CALIB_N: AtomicU64 = AtomicU64::new(0);
pub static SNAP_ON_E: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    fn clock_gettime_nsec_np(clock: u32) -> u64;
    fn pthread_set_qos_class_self_np(qos: u32, rel: i32) -> i32;
    fn pthread_self() -> usize;
    fn pthread_get_qos_class_np(t: usize, qos: *mut u32, rel: *mut i32) -> i32;
}
const CLOCK_THREAD_CPUTIME_ID: u32 = 16;

pub fn thread_cpu_ns() -> u64 {
    unsafe { clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID) }
}

pub fn on_ecore() -> bool {
    let v: u64;
    unsafe { std::arch::asm!("mrs {}, TPIDR_EL0", out(reg) v) };
    v & 0xfff < 0x100
}

pub fn qos_name() -> &'static str {
    let (mut q, mut r) = (0u32, 0i32);
    unsafe { pthread_get_qos_class_np(pthread_self(), &mut q, &mut r) };
    match q {
        0x21 => "user-interactive",
        0x19 => "user-initiated",
        0x15 => "default",
        0x11 => "utility",
        0x09 => "background",
        _ => "unspecified",
    }
}

/// `SPIKE_FEEDER_QOS=ui|initiated|utility` pins the feeder thread's QoS.
pub fn apply_feeder_qos() {
    let q = match std::env::var("SPIKE_FEEDER_QOS").ok().as_deref() {
        Some("ui") => 0x21,
        Some("initiated") => 0x19,
        Some("utility") => 0x11,
        _ => return,
    };
    unsafe { pthread_set_qos_class_self_np(q, 0) };
}

pub fn ns(t: Instant) -> u64 {
    t.elapsed().as_nanos() as u64
}

pub fn add(c: &AtomicU64, v: u64) {
    c.fetch_add(v, Relaxed);
}

pub fn get(c: &AtomicU64) -> u64 {
    c.load(Relaxed)
}

/// A fixed register-only ALU loop (no memory traffic): its wall time
/// tracks the core's clock, so a ratio between runs is a clock ratio.
pub fn calibrate() {
    let t0 = Instant::now();
    let mut x: u64 = 0x9e37_79b9;
    for _ in 0..20_000 {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407));
    }
    std::hint::black_box(x);
    add(&CALIB_NS, ns(t0));
    add(&CALIB_N, 1);
}
