//! GPUI side of the spike: the view owns no terminal state, it renders
//! [`RenderSnapshot`]s and turns input events into bytes. Engine events are
//! drained with mandatory coalescing: after a blocking wait for the first
//! event, the drain collects for at most 4 ms / 100 events and then fires
//! exactly one `cx.notify()`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::backend::alacritty::EventStream;
use crate::backend::{
    keys, CursorShape, ExitStatus, RenderSnapshot, Rgba, StyleFlags, TerminalBackend,
    TerminalEvent, DEFAULT_BG, DEFAULT_FG,
};

/// How long the drain keeps collecting after the first event of a batch.
const COALESCE_WINDOW: Duration = Duration::from_millis(4);
/// Max events per coalesced batch.
const COALESCE_MAX: usize = 100;

/// Pending latency-probe measurement (SPIKE_LATENCY_PROBE).
pub struct Probe {
    pub marker: String,
    pub start: Instant,
}

pub struct TerminalView {
    backend: Arc<dyn TerminalBackend>,
    focus_handle: FocusHandle,
    /// normal / bold / italic / bold+italic
    fonts: [Font; 4],
    font_size: Pixels,
    cell_width: Pixels,
    cell_height: Pixels,
    grid_size: (usize, usize),
    /// Fractional pixel-scroll accumulator.
    scroll_acc: f32,
    title: Option<String>,
    child_exited: Option<ExitStatus>,
    /// Wakeups received from the engine vs notifies issued to GPUI.
    pub wakeup_events: u64,
    pub notifies: u64,
    pub probe: Arc<Mutex<Option<Probe>>>,
}

impl TerminalView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        backend: Arc<dyn TerminalBackend>,
        events: Arc<EventStream>,
        auto: bool,
    ) -> Self {
        let font_size = px(13.0);
        let base: Font = font("Menlo");
        let bold = Font {
            weight: FontWeight::BOLD,
            ..base.clone()
        };
        let italic = Font {
            style: FontStyle::Italic,
            ..base.clone()
        };
        let bold_italic = Font {
            weight: FontWeight::BOLD,
            style: FontStyle::Italic,
            ..base.clone()
        };

        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&base);
        let cell_width = text_system
            .advance(font_id, font_size, 'M')
            .map(|s| s.width)
            .unwrap_or(px(8.0));
        let ascent = text_system.ascent(font_id, font_size);
        let descent = text_system.descent(font_id, font_size);
        // A little leading, like a real terminal.
        let cell_height = ((ascent + descent) * 1.15).ceil();

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let view = Self {
            backend,
            focus_handle,
            fonts: [base, bold, italic, bold_italic],
            font_size,
            cell_width,
            cell_height,
            grid_size: (0, 0),
            scroll_acc: 0.0,
            title: None,
            child_exited: None,
            wakeup_events: 0,
            notifies: 0,
            probe: Arc::new(Mutex::new(None)),
        };
        view.spawn_event_drain(cx, events);
        if auto {
            crate::auto::spawn_driver(cx);
        }
        view
    }

    pub fn backend(&self) -> &Arc<dyn TerminalBackend> {
        &self.backend
    }

    pub fn child_exited(&self) -> Option<ExitStatus> {
        self.child_exited
    }

    /// Background-recv + bounded foreground drain; one notify per batch.
    fn spawn_event_drain(&self, cx: &mut Context<Self>, events: Arc<EventStream>) {
        cx.spawn(async move |this, cx| loop {
            let first = {
                let events = events.clone();
                cx.background_executor()
                    .spawn(async move { events.recv() })
                    .await
            };
            let Some(first) = first else { break };
            let mut batch = vec![first];

            let mut rest = {
                let events = events.clone();
                cx.background_executor()
                    .spawn(async move {
                        let deadline = Instant::now() + COALESCE_WINDOW;
                        let mut v = Vec::new();
                        while v.len() + 1 < COALESCE_MAX {
                            match events.try_recv() {
                                Some(e) => v.push(e),
                                None if Instant::now() < deadline => {
                                    std::thread::sleep(Duration::from_millis(1))
                                }
                                None => break,
                            }
                        }
                        v
                    })
                    .await
            };
            batch.append(&mut rest);

            if this
                .update(cx, |view, cx| view.handle_events(batch, cx))
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    fn handle_events(&mut self, batch: Vec<TerminalEvent>, cx: &mut Context<Self>) {
        for event in batch {
            match event {
                TerminalEvent::Wakeup => self.wakeup_events += 1,
                TerminalEvent::Title(title) => {
                    println!("[event] title: {title}");
                    self.title = Some(title);
                }
                TerminalEvent::ResetTitle => self.title = None,
                TerminalEvent::Bell => {}
                TerminalEvent::PtyWrite(bytes) => self.backend.write_input(&bytes),
                TerminalEvent::ClipboardStore { text, .. } => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                TerminalEvent::ClipboardLoad { .. } => {
                    let text = cx
                        .read_from_clipboard()
                        .and_then(|item| item.text())
                        .unwrap_or_default();
                    self.backend.respond_clipboard_load(&text);
                }
                TerminalEvent::ColorRequest { index } => {
                    println!("[event] color request {index} (unanswered)");
                }
                TerminalEvent::ChildExit(status) => {
                    println!("[event] child exited: {status:?}");
                    self.child_exited = Some(status);
                }
                TerminalEvent::Exit => {
                    self.child_exited = Some(ExitStatus {
                        code: None,
                        signal: None,
                    });
                }
                TerminalEvent::CursorBlinkingChange | TerminalEvent::MouseCursorDirty => {}
            }
        }
        self.notifies += 1;
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;

        // Platform shortcuts: cmd-v paste. (cmd-c needs selection — out of spike scope.)
        if ks.modifiers.platform {
            if ks.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let modes = self.backend.modes();
                    self.backend.scroll_to_bottom();
                    self.backend.write_input(&keys::encode_paste(&text, &modes));
                }
                cx.stop_propagation();
            }
            return;
        }

        let mods = keys::Mods {
            ctrl: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
        };
        let Some(key) = map_key(ks) else { return };
        let modes = self.backend.modes();
        if let Some(bytes) = keys::encode_key(key, mods, &modes) {
            self.backend.scroll_to_bottom();
            self.backend.write_input(&bytes);
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lines: i32 = match event.delta {
            ScrollDelta::Lines(p) => p.y.round() as i32,
            ScrollDelta::Pixels(p) => {
                let cell_h = f32::from(self.cell_height);
                self.scroll_acc += f32::from(p.y);
                let whole = (self.scroll_acc / cell_h).trunc();
                self.scroll_acc -= whole * cell_h;
                whole as i32
            }
        };
        if lines == 0 {
            return;
        }
        let modes = self.backend.modes();
        if modes.alt_screen && !modes.mouse_report {
            // Alternate scroll: turn the wheel into arrow keys for less/vim.
            let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            for _ in 0..lines.abs() {
                self.backend.write_input(seq);
            }
        } else {
            self.backend.scroll_display(lines);
            cx.notify();
        }
        cx.stop_propagation();
    }

    /// Recompute grid size from the viewport; resize engine + PTY on change.
    fn sync_size(&mut self, window: &mut Window) {
        let viewport = window.viewport_size();
        let cols = (f32::from(viewport.width) / f32::from(self.cell_width))
            .floor()
            .max(2.0);
        let rows = (f32::from(viewport.height) / f32::from(self.cell_height))
            .floor()
            .max(1.0);
        let size = (cols as usize, rows as usize);
        if size != self.grid_size {
            self.grid_size = size;
            self.backend.resize(
                size.0,
                size.1,
                f32::from(self.cell_width) as u16,
                f32::from(self.cell_height) as u16,
            );
        }
    }
}

fn map_key(ks: &Keystroke) -> Option<keys::Key> {
    use keys::Key::*;
    let single = |s: &str| {
        let mut chars = s.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    };
    Some(match ks.key.as_str() {
        "enter" => Enter,
        "backspace" => Backspace,
        "tab" => Tab,
        "escape" => Escape,
        "up" => Up,
        "down" => Down,
        "left" => Left,
        "right" => Right,
        "home" => Home,
        "end" => End,
        "pageup" => PageUp,
        "pagedown" => PageDown,
        "insert" => Insert,
        "delete" => Delete,
        "space" => Char(' '),
        name if name.len() >= 2 && name.starts_with('f') => {
            let n: u8 = name[1..].parse().ok()?;
            if (1..=12).contains(&n) {
                F(n)
            } else {
                return None;
            }
        }
        _ => {
            if ks.modifiers.control || ks.modifiers.alt {
                Char(single(&ks.key)?)
            } else if let Some(key_char) = &ks.key_char {
                Char(single(key_char)?)
            } else {
                Char(single(&ks.key)?)
            }
        }
    })
}

fn to_hsla(c: Rgba) -> Hsla {
    rgba(((c.r as u32) << 24) | ((c.g as u32) << 16) | ((c.b as u32) << 8) | c.a as u32).into()
}

/// Which of the four font variants a cell needs.
fn font_index(flags: StyleFlags) -> usize {
    let bold = flags.contains(StyleFlags::BOLD);
    let italic = flags.contains(StyleFlags::ITALIC);
    (bold as usize) | ((italic as usize) << 1)
}

#[derive(Clone, Copy, PartialEq)]
enum Deco {
    None,
    Underline { wavy: bool },
    Strike,
}

/// A row shaped for painting, plus the snapshot it came from.
struct Prepared {
    snapshot: RenderSnapshot,
    lines: Vec<Option<ShapedLine>>,
}

fn shape_rows(
    snapshot: RenderSnapshot,
    fonts: &[Font; 4],
    font_size: Pixels,
    window: &mut Window,
) -> Prepared {
    let text_system = window.text_system().clone();
    let mut lines = Vec::with_capacity(snapshot.rows);

    for row in 0..snapshot.rows {
        let mut text = String::new();
        let mut runs: Vec<TextRun> = Vec::new();
        let mut visible = false;
        // (font variant, fg, decoration) of the current open run.
        let mut key: Option<(usize, Rgba, Deco)> = None;

        let mut push = |c: char, flags: StyleFlags, fg: Rgba| {
            if c != ' ' {
                visible = true;
            }
            let deco = if flags.intersects(StyleFlags::UNDERLINE | StyleFlags::DOUBLE_UNDERLINE) {
                Deco::Underline { wavy: false }
            } else if flags.contains(StyleFlags::UNDERCURL) {
                Deco::Underline { wavy: true }
            } else if flags.contains(StyleFlags::STRIKEOUT) {
                Deco::Strike
            } else {
                Deco::None
            };
            let new_key = (font_index(flags), fg, deco);
            if key != Some(new_key) {
                key = Some(new_key);
                runs.push(TextRun {
                    len: 0,
                    font: fonts[new_key.0].clone(),
                    color: to_hsla(fg),
                    background_color: None,
                    underline: match deco {
                        Deco::Underline { wavy } => Some(UnderlineStyle {
                            thickness: px(1.0),
                            color: Some(to_hsla(fg)),
                            wavy,
                        }),
                        _ => None,
                    },
                    strikethrough: match deco {
                        Deco::Strike => Some(StrikethroughStyle {
                            thickness: px(1.0),
                            color: Some(to_hsla(fg)),
                        }),
                        _ => None,
                    },
                });
            }
            runs.last_mut().unwrap().len += c.len_utf8();
            text.push(c);
        };

        for col in 0..snapshot.cols {
            let cell = snapshot.cell(col, row);
            if cell.flags.contains(StyleFlags::WIDE_CHAR_SPACER) {
                continue;
            }
            let c = if cell.flags.contains(StyleFlags::HIDDEN) {
                ' '
            } else {
                cell.c
            };
            push(c, cell.flags, cell.fg);
            if let Some(zw) = &cell.combining {
                for &zc in zw {
                    push(zc, cell.flags, cell.fg);
                }
            }
        }

        if visible && !text.is_empty() {
            lines.push(Some(text_system.shape_line(
                SharedString::from(text),
                font_size,
                &runs,
                None,
            )));
        } else {
            lines.push(None);
        }
    }

    Prepared { snapshot, lines }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_size(window);

        let backend = self.backend.clone();
        let probe = self.probe.clone();
        let fonts = self.fonts.clone();
        let font_size = self.font_size;
        let cell_width = self.cell_width;
        let cell_height = self.cell_height;

        let grid = canvas(
            move |_bounds, window, _cx| {
                let snapshot = backend.snapshot();
                shape_rows(snapshot, &fonts, font_size, window)
            },
            move |bounds, prepared, window, cx| {
                paint_grid(bounds, &prepared, cell_width, cell_height, window, cx);
                if std::env::var("SPIKE_DEBUG_PAINT").is_ok() {
                    use std::sync::atomic::{AtomicBool, Ordering};
                    static PRINTED: AtomicBool = AtomicBool::new(false);
                    if !PRINTED.swap(true, Ordering::Relaxed) {
                        println!("[debug] canvas paint executed");
                    }
                }
                // Latency probe: did this frame make the marker visible?
                let mut probe = probe.lock().unwrap();
                if let Some(p) = probe.as_ref() {
                    if prepared.snapshot.contains(&p.marker) {
                        println!("[probe] keystroke->paint: {:?}", p.start.elapsed());
                        *probe = None;
                    }
                }
            },
        )
        .size_full();

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(to_hsla(DEFAULT_BG))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, window, cx| {
                    this.focus_handle.focus(window, cx);
                }),
            )
            .child(grid)
    }
}

fn paint_grid(
    bounds: Bounds<Pixels>,
    prepared: &Prepared,
    cell_width: Pixels,
    cell_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let snapshot = &prepared.snapshot;
    let default_fg = to_hsla(DEFAULT_FG);
    let origin = bounds.origin;

    // 1. Background runs (skip the default color; the window bg shows through).
    for row in 0..snapshot.rows {
        let y = origin.y + cell_height * row as f32;
        let mut col = 0;
        while col < snapshot.cols {
            let bg = snapshot.cell(col, row).bg;
            if bg == DEFAULT_BG {
                col += 1;
                continue;
            }
            let start = col;
            while col < snapshot.cols && snapshot.cell(col, row).bg == bg {
                col += 1;
            }
            window.paint_quad(fill(
                Bounds {
                    origin: point(origin.x + cell_width * start as f32, y),
                    size: gpui_kit::size(cell_width * (col - start) as f32, cell_height),
                },
                to_hsla(bg),
            ));
        }
    }

    // 2. Block cursor goes UNDER the text (glyph stays readable).
    if let Some(cursor) = snapshot.cursor {
        let x = origin.x + cell_width * cursor.col as f32;
        let y = origin.y + cell_height * cursor.row as f32;
        if cursor.shape == CursorShape::Block {
            let mut color = DEFAULT_FG;
            color.a = 0x66;
            window.paint_quad(fill(
                Bounds {
                    origin: point(x, y),
                    size: gpui_kit::size(cell_width, cell_height),
                },
                to_hsla(color),
            ));
        }
    }

    // 3. Text.
    for (row, line) in prepared.lines.iter().enumerate() {
        if let Some(line) = line {
            let y = origin.y + cell_height * row as f32;
            let _ = line.paint(
                point(origin.x, y),
                cell_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }

    // 4. Non-block cursor variants over the text.
    if let Some(cursor) = snapshot.cursor {
        let x = origin.x + cell_width * cursor.col as f32;
        let y = origin.y + cell_height * cursor.row as f32;
        match cursor.shape {
            CursorShape::Beam => {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(x, y),
                        size: gpui_kit::size(px(2.0), cell_height),
                    },
                    default_fg,
                ));
            }
            CursorShape::Underline => {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(x, y + cell_height - px(2.0)),
                        size: gpui_kit::size(cell_width, px(2.0)),
                    },
                    default_fg,
                ));
            }
            CursorShape::HollowBlock => {
                let mut quad = fill(
                    Bounds {
                        origin: point(x, y),
                        size: gpui_kit::size(cell_width, cell_height),
                    },
                    rgba(0x00000000),
                );
                quad.border_widths = Edges::all(px(1.0));
                quad.border_color = default_fg;
                window.paint_quad(quad);
            }
            CursorShape::Block | CursorShape::Hidden => {}
        }
    }
}
