//! Astrolabe desktop application: GPUI entry point.
//!
//! The store and session entities live in this crate (per the workspace
//! decision, they are main-thread-resident GPUI entities — no trait
//! boundary). `astrolabe-kube` (cluster I/O) and `astrolabe-terminal`
//! (terminal engine + view) plug in here.

use gpui_kit::*;

/// Placeholder root view until the DockArea / sidebar / resource views land.
struct RootView;

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("Astrolabe")
    }
}

fn main() {
    gpui_kit::application().run(|cx| {
        cx.open_window(WindowOptions::default(), |_window, cx| {
            cx.new(|_cx| RootView)
        })
        .expect("failed to open window");

        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}
