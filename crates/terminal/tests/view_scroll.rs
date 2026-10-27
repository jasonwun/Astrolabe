//! `TerminalView` scroll-wheel handling, driven through real event dispatch.

use std::sync::Arc;

use astrolabe_terminal::TerminalBackend;
use astrolabe_terminal::TerminalView;
use astrolabe_terminal::backend::display_only;
use gpui_kit::{ScrollDelta, ScrollEvent, TestAppContext, point, px};

/// The wheel scrolls the display into scrollback: positive y moves up.
#[gpui_kit::test]
fn wheel_scrolls_the_display_into_scrollback(cx: &mut TestAppContext) {
    let (backend, events) = display_only(40, 10, 1000);
    let backend = Arc::new(backend);
    let lines: String = (1..=300).map(|n| format!("line {n}\r\n")).collect();
    backend.feed_output(lines.as_bytes());
    let view_backend: Arc<dyn TerminalBackend> = backend.clone();
    let (_view, cx) = cx.add_window_view(|window, cx| {
        TerminalView::new(window, cx, view_backend, Arc::new(events))
    });
    cx.run_until_parked();
    assert_eq!(backend.snapshot().display_offset, 0);

    cx.simulate_event(ScrollEvent {
        position: point(px(50.), px(50.)),
        delta: ScrollDelta::Lines(point(0., 5.)),
        ..Default::default()
    });
    cx.run_until_parked();

    assert_eq!(backend.snapshot().display_offset, 5);
}
