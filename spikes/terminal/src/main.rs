//! PROTOTYPE / THROWAWAY — alacritty_terminal grid + GPUI self-rendered
//! terminal spike (issue #7). Not production code.
//!
//! `cargo run --release` opens an interactive terminal window running your
//! `$SHELL`. `SPIKE_AUTO=1 cargo run --release` runs the scripted smoke
//! suite against `/bin/sh` and quits. See README.md.

// The backend interface sketches types for the real terminal (modes,
// clipboard kinds, display-only constructor); not every field is read by
// the spike itself.
#![allow(dead_code)]

mod auto;
mod backend;
mod view;

use std::sync::Arc;

use gpui_kit::*;

use backend::{spawn_shell, SessionConfig, TerminalBackend};
use view::TerminalView;

fn main() {
    let auto = std::env::var("SPIKE_AUTO").ok().as_deref() == Some("1");

    // Turn on gpui's frame-timing trace for the flood-phase draw stats.
    gpui_kit::profiler::set_trace_enabled(true);

    gpui_kit::application().run(move |cx| {
        let (program, env): (String, Vec<(String, String)>) = if auto {
            // Deterministic shell for the scripted suite: POSIX sh, no rc
            // files, fixed prompt.
            (
                "/bin/sh".to_string(),
                vec![
                    ("TERM".to_string(), "xterm-256color".to_string()),
                    ("COLORTERM".to_string(), "truecolor".to_string()),
                    ("ENV".to_string(), "/dev/null".to_string()),
                    ("PS1".to_string(), "spike$ ".to_string()),
                ],
            )
        } else {
            (
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string()),
                vec![
                    ("TERM".to_string(), "xterm-256color".to_string()),
                    ("COLORTERM".to_string(), "truecolor".to_string()),
                ],
            )
        };

        let config = SessionConfig {
            program,
            args: vec![], // a shell on a PTY is interactive by default
            env,
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 17,
            scrollback: 10_000,
        };

        let (backend, events) = spawn_shell(&config);
        let backend: Arc<dyn TerminalBackend> = Arc::new(backend);
        let events = Arc::new(events);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(100.0), px(100.0)),
                    size: size(px(960.0), px(640.0)),
                })),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| TerminalView::new(window, cx, backend, events, auto)),
        )
        .expect("failed to open window");

        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}
