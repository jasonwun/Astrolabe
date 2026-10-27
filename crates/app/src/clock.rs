//! The wall clock that panels project ages from (#70). The app reads the
//! system time; a UI test installs a [`Clock`] that follows its scheduler,
//! so advancing the test clock advances every displayed age.

use std::rc::Rc;

use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use gpui_kit::{App, Global};

/// Replaces the system time for every panel in this app.
#[derive(Clone)]
pub struct Clock(Rc<dyn Fn(&App) -> Timestamp>);

impl Global for Clock {}

impl Clock {
    pub fn new(now: impl Fn(&App) -> Timestamp + 'static) -> Self {
        Self(Rc::new(now))
    }
}

/// The installed [`Clock`]'s time, or the system time without one.
pub fn now(cx: &App) -> Timestamp {
    cx.try_global::<Clock>()
        .map_or_else(Timestamp::now, |clock| (clock.0)(cx))
}
