//! `alacritty_terminal` implementation of the engine boundary.
//!
//! This is the ONLY module in the crate allowed to name alacritty/vte
//! types. PTY IO is driven by alacritty's own `event_loop::EventLoop`
//! (a plain sync thread, polling-based — no tokio anywhere). A
//! replacement engine (hand-written reader/writer threads, another VT
//! crate, …) can slot in behind `backend/mod.rs` without touching
//! anything outside this file.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{self, ClipboardType, Term, TermDamage, TermMode};
use alacritty_terminal::tty::{self, Options, Shell};
use alacritty_terminal::vte::ansi::{self, Color, CursorShape as VteCursorShape, NamedColor};

use super::{
    BackendStats, ClipboardKind, CursorShape, DEFAULT_BG, DEFAULT_FG, ExitStatus, RemoteSink,
    RenderSnapshot, Rgba, SessionConfig, SnapshotCell, SnapshotCursor, StyleFlags, TerminalBackend,
    TerminalEvent, TerminalModes,
};

/// Formatter carried by alacritty's OSC52-load event; never leaves this module.
type ClipboardFormatter = Arc<dyn Fn(&str) -> String + Sync + Send + 'static>;

/// Shared state between the engine's event listener(s) and the backend.
struct Shared {
    tx: Sender<TerminalEvent>,
    /// The task waiting in [`EventStream::next`], if any.
    waker: Arc<Mutex<Option<Waker>>>,
    pending_clipboard: Mutex<Option<(ClipboardKind, ClipboardFormatter)>>,
    wakeups: AtomicU64,
    damage_full: AtomicU64,
    damage_partial_lines: AtomicU64,
    snapshots: AtomicU64,
}

impl Shared {
    fn new(tx: Sender<TerminalEvent>, waker: Arc<Mutex<Option<Waker>>>) -> Self {
        Self {
            tx,
            waker,
            pending_clipboard: Mutex::new(None),
            wakeups: AtomicU64::new(0),
            damage_full: AtomicU64::new(0),
            damage_partial_lines: AtomicU64::new(0),
            snapshots: AtomicU64::new(0),
        }
    }

    fn emit(&self, event: TerminalEvent) {
        if matches!(event, TerminalEvent::Wakeup) {
            self.wakeups.fetch_add(1, Ordering::Relaxed);
        }
        // A dead UI must never wedge the PTY reader thread.
        let _ = self.tx.send(event);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

/// alacritty's `EventListener` over a channel. Cloned: one copy lives in
/// the `Term`, one in the `EventLoop`.
#[derive(Clone)]
struct SharedListener {
    shared: Arc<Shared>,
}

impl EventListener for SharedListener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => self.shared.emit(TerminalEvent::Wakeup),
            Event::Title(t) => self.shared.emit(TerminalEvent::Title(t)),
            Event::ResetTitle => self.shared.emit(TerminalEvent::ResetTitle),
            Event::Bell => self.shared.emit(TerminalEvent::Bell),
            Event::PtyWrite(text) => self.shared.emit(TerminalEvent::PtyWrite(text.into_bytes())),
            Event::ClipboardStore(ty, text) => self.shared.emit(TerminalEvent::ClipboardStore {
                clipboard: clipboard_kind(ty),
                text,
            }),
            Event::ClipboardLoad(ty, formatter) => {
                let kind = clipboard_kind(ty);
                *self.shared.pending_clipboard.lock().unwrap() = Some((kind, formatter));
                self.shared
                    .emit(TerminalEvent::ClipboardLoad { clipboard: kind });
            }
            Event::ColorRequest(index, _formatter) => {
                // Not answered yet (OSC 10/11/12 color queries).
                self.shared.emit(TerminalEvent::ColorRequest { index });
            }
            Event::ChildExit(status) => self
                .shared
                .emit(TerminalEvent::ChildExit(convert_exit_status(status))),
            Event::Exit => self.shared.emit(TerminalEvent::Exit),
            Event::CursorBlinkingChange => self.shared.emit(TerminalEvent::CursorBlinkingChange),
            Event::MouseCursorDirty => self.shared.emit(TerminalEvent::MouseCursorDirty),
            Event::TextAreaSizeRequest(_formatter) => {
                // Unhandled (very few apps query this).
            }
        }
    }
}

fn clipboard_kind(ty: ClipboardType) -> ClipboardKind {
    match ty {
        ClipboardType::Clipboard => ClipboardKind::Clipboard,
        ClipboardType::Selection => ClipboardKind::Selection,
    }
}

fn convert_exit_status(status: std::process::ExitStatus) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus {
            code: status.code(),
            signal: status.signal(),
        }
    }
    #[cfg(not(unix))]
    {
        ExitStatus {
            code: status.code(),
            signal: None,
        }
    }
}

/// Receiving end of the session's event stream (own event type only).
///
/// `recv` blocks and must be called from a background thread; `next`
/// waits without blocking a thread; `try_recv` is for the coalescing drain
/// on the UI side.
pub struct EventStream {
    rx: Mutex<Receiver<TerminalEvent>>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl EventStream {
    /// Wait for the next event without blocking a thread; `None` when the
    /// session is gone.
    pub async fn next(&self) -> Option<TerminalEvent> {
        std::future::poll_fn(|cx| {
            match self.rx.lock().unwrap().try_recv() {
                Ok(event) => return Poll::Ready(Some(event)),
                Err(mpsc::TryRecvError::Disconnected) => return Poll::Ready(None),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            *self.waker.lock().unwrap() = Some(cx.waker().clone());
            // An event sent between the check and the registration found
            // no waker to wake: look again.
            match self.rx.lock().unwrap().try_recv() {
                Ok(event) => Poll::Ready(Some(event)),
                Err(mpsc::TryRecvError::Disconnected) => Poll::Ready(None),
                Err(mpsc::TryRecvError::Empty) => Poll::Pending,
            }
        })
        .await
    }

    /// Block until the next event; `None` when the session is gone.
    pub fn recv(&self) -> Option<TerminalEvent> {
        self.rx.lock().unwrap().recv().ok()
    }

    pub fn try_recv(&self) -> Option<TerminalEvent> {
        self.rx.lock().unwrap().try_recv().ok()
    }
}

/// Grid dimensions as alacritty wants them.
#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct AlacrittyBackend {
    term: Arc<FairMutex<Term<SharedListener>>>,
    shared: Arc<Shared>,
    /// None for display-only terminals.
    io: Mutex<Option<IoHandle>>,
    /// VT parser for display-only and remote terminals.
    parser: Mutex<Option<ansi::Processor>>,
    /// The far end of a remote terminal, until shutdown.
    remote: Mutex<Option<Box<dyn RemoteSink>>>,
    last_size: Mutex<(usize, usize)>,
}

struct IoHandle {
    sender: EventLoopSender,
    joiner: JoinHandle<()>,
}

fn make_term(
    cols: usize,
    rows: usize,
    scrollback: usize,
) -> (
    Arc<FairMutex<Term<SharedListener>>>,
    Arc<Shared>,
    EventStream,
) {
    let (tx, rx) = mpsc::channel();
    let waker = Arc::new(Mutex::new(None));
    let shared = Arc::new(Shared::new(tx, waker.clone()));
    let listener = SharedListener {
        shared: shared.clone(),
    };
    let config = term::Config {
        scrolling_history: scrollback,
        ..Default::default()
    };
    let term = Term::new(config, &Size { cols, rows }, listener);
    (
        Arc::new(FairMutex::new(term)),
        shared,
        EventStream {
            rx: Mutex::new(rx),
            waker,
        },
    )
}

impl AlacrittyBackend {
    /// Route A: spawn a PTY with a shell, driven by alacritty's own
    /// event loop thread.
    pub fn spawn(config: &SessionConfig) -> (Self, EventStream) {
        let (term, shared, events) = make_term(config.cols, config.rows, config.scrollback);
        let listener = SharedListener {
            shared: shared.clone(),
        };

        let env: HashMap<String, String> = config.env.iter().cloned().collect();
        let options = Options {
            shell: Some(Shell::new(config.program.clone(), config.args.clone())),
            working_directory: None,
            drain_on_exit: false,
            env,
        };
        let window_size = WindowSize {
            num_lines: config.rows as u16,
            num_cols: config.cols as u16,
            cell_width: config.cell_width,
            cell_height: config.cell_height,
        };
        let pty = tty::new(&options, window_size, 0).expect("failed to spawn PTY");
        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)
            .expect("failed to create event loop");
        let sender = event_loop.channel();
        let loop_thread = event_loop.spawn();
        // Detach the unnameable JoinHandle<(EventLoop, State)> behind a
        // plain joiner thread; when the loop ends the returned EventLoop
        // (and with it the PTY master fd) is dropped, SIGHUPing the child.
        let joiner = std::thread::Builder::new()
            .name("pty-loop-join".into())
            .spawn(move || {
                let _ = loop_thread.join();
            })
            .expect("failed to spawn joiner thread");

        let backend = Self {
            term,
            shared,
            io: Mutex::new(Some(IoHandle { sender, joiner })),
            parser: Mutex::new(None),
            remote: Mutex::new(None),
            last_size: Mutex::new((config.cols, config.rows)),
        };
        (backend, events)
    }

    /// Display-only terminal (log streams, tests): feed bytes via
    /// [`AlacrittyBackend::advance`]. `write_input` is a no-op.
    pub fn display_only(cols: usize, rows: usize, scrollback: usize) -> (Self, EventStream) {
        let (term, shared, events) = make_term(cols, rows, scrollback);
        let backend = Self {
            term,
            shared,
            io: Mutex::new(None),
            parser: Mutex::new(Some(ansi::Processor::default())),
            remote: Mutex::new(None),
            last_size: Mutex::new((cols, rows)),
        };
        (backend, events)
    }

    /// Remote terminal (`kubectl exec`): output is fed like a display-only
    /// terminal's; input and resizes go to `sink`, which first learns the
    /// starting size.
    pub fn remote(
        cols: usize,
        rows: usize,
        scrollback: usize,
        sink: Box<dyn RemoteSink>,
    ) -> (Self, EventStream) {
        let (backend, events) = Self::display_only(cols, rows, scrollback);
        sink.resize(cols as u16, rows as u16);
        *backend.remote.lock().unwrap() = Some(sink);
        (backend, events)
    }

    /// The remote process ended with `status`.
    pub fn remote_exited(&self, status: ExitStatus) {
        self.shared.emit(TerminalEvent::ChildExit(status));
    }

    /// Feed an ANSI byte stream into a display-only terminal.
    /// Panics on a PTY-backed terminal (tests use this directly).
    pub fn advance(&self, bytes: &[u8]) {
        assert!(
            self.parser.lock().unwrap().is_some(),
            "advance on a PTY-backed terminal"
        );
        self.feed_output(bytes);
    }
}

impl TerminalBackend for AlacrittyBackend {
    fn feed_output(&self, bytes: &[u8]) {
        let mut parser = self.parser.lock().unwrap();
        let Some(parser) = parser.as_mut() else {
            return;
        };
        {
            let mut term = self.term.lock();
            parser.advance(&mut *term, bytes);
        }
        self.shared.emit(TerminalEvent::Wakeup);
    }

    fn write_input(&self, bytes: &[u8]) {
        if let Some(io) = self.io.lock().unwrap().as_ref() {
            let _ = io.sender.send(Msg::Input(Cow::Owned(bytes.to_vec())));
        }
        if let Some(remote) = self.remote.lock().unwrap().as_ref() {
            remote.input(bytes);
        }
    }

    fn resize(&self, cols: usize, rows: usize, cell_width: u16, cell_height: u16) {
        {
            let mut last = self.last_size.lock().unwrap();
            if *last == (cols, rows) {
                return;
            }
            *last = (cols, rows);
        }
        self.term.lock().resize(Size { cols, rows });
        if let Some(io) = self.io.lock().unwrap().as_ref() {
            let _ = io.sender.send(Msg::Resize(WindowSize {
                num_lines: rows as u16,
                num_cols: cols as u16,
                cell_width,
                cell_height,
            }));
        }
        if let Some(remote) = self.remote.lock().unwrap().as_ref() {
            remote.resize(cols as u16, rows as u16);
        }
    }

    fn scroll_display(&self, delta_lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(delta_lines));
    }

    fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    fn snapshot(&self) -> RenderSnapshot {
        let mut term = self.term.lock();
        let content = term.renderable_content();
        let cols = term.grid().columns();
        let rows = term.grid().screen_lines();
        let display_offset = content.display_offset;

        let mut cells = Vec::with_capacity(cols * rows);
        for indexed in content.display_iter {
            let cell = indexed.cell;
            let f = cell.flags;

            let mut flags = StyleFlags::NONE;
            for (engine, own) in [
                (Flags::BOLD, StyleFlags::BOLD),
                (Flags::ITALIC, StyleFlags::ITALIC),
                (Flags::DIM, StyleFlags::DIM),
                (Flags::UNDERLINE, StyleFlags::UNDERLINE),
                (Flags::DOUBLE_UNDERLINE, StyleFlags::DOUBLE_UNDERLINE),
                (Flags::UNDERCURL, StyleFlags::UNDERCURL),
                (Flags::STRIKEOUT, StyleFlags::STRIKEOUT),
                (Flags::WIDE_CHAR, StyleFlags::WIDE_CHAR),
                (Flags::WIDE_CHAR_SPACER, StyleFlags::WIDE_CHAR_SPACER),
                (Flags::WRAPLINE, StyleFlags::WRAPLINE),
                (Flags::HIDDEN, StyleFlags::HIDDEN),
            ] {
                if f.intersects(engine) {
                    flags = flags | own;
                }
            }

            let mut fg = resolve_color(cell.fg, content.colors, f.contains(Flags::DIM));
            let mut bg = resolve_color(cell.bg, content.colors, false);
            if f.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }

            cells.push(SnapshotCell {
                c: cell.c,
                combining: cell.zerowidth().map(|zs| zs.to_vec()),
                fg,
                bg,
                flags,
            });
        }
        // The iterator should yield exactly cols*rows cells; defend anyway.
        while cells.len() < cols * rows {
            cells.push(SnapshotCell {
                c: ' ',
                combining: None,
                fg: DEFAULT_FG,
                bg: DEFAULT_BG,
                flags: StyleFlags::NONE,
            });
        }
        cells.truncate(cols * rows);

        let cursor = match content.cursor.shape {
            VteCursorShape::Hidden => None,
            shape => {
                let p = content.cursor.point;
                let row = p.line.0 + display_offset as i32;
                (row >= 0 && (row as usize) < rows).then(|| SnapshotCursor {
                    col: p.column.0,
                    row: row as usize,
                    shape: convert_cursor_shape(shape),
                })
            }
        };

        let history_size = term.grid().history_size();
        let alt_screen = content.mode.contains(TermMode::ALT_SCREEN);

        // Damage bookkeeping (rendering is full-grid for now).
        self.shared.snapshots.fetch_add(1, Ordering::Relaxed);
        match term.damage() {
            TermDamage::Full => {
                self.shared.damage_full.fetch_add(1, Ordering::Relaxed);
            }
            TermDamage::Partial(lines) => {
                self.shared
                    .damage_partial_lines
                    .fetch_add(lines.count() as u64, Ordering::Relaxed);
            }
        }
        term.reset_damage();

        RenderSnapshot {
            cols,
            rows,
            cells,
            cursor,
            display_offset,
            history_size,
            alt_screen,
        }
    }

    fn modes(&self) -> TerminalModes {
        let term = self.term.lock();
        let mode = *term.mode();
        TerminalModes {
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            show_cursor: mode.contains(TermMode::SHOW_CURSOR),
            mouse_report: mode.intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE),
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
            line_wrap: mode.contains(TermMode::LINE_WRAP),
        }
    }

    fn respond_clipboard_load(&self, text: &str) {
        let pending = self.shared.pending_clipboard.lock().unwrap().take();
        if let Some((_kind, formatter)) = pending {
            self.write_input(formatter(text).as_bytes());
        }
    }

    fn stats(&self) -> BackendStats {
        BackendStats {
            wakeups: self.shared.wakeups.load(Ordering::Relaxed),
            damage_full: self.shared.damage_full.load(Ordering::Relaxed),
            damage_partial_lines: self.shared.damage_partial_lines.load(Ordering::Relaxed),
            snapshots: self.shared.snapshots.load(Ordering::Relaxed),
        }
    }

    fn shutdown(&self) {
        if let Some(io) = self.io.lock().unwrap().take() {
            let _ = io.sender.send(Msg::Shutdown);
            let _ = io.joiner.join();
        }
        if let Some(remote) = self.remote.lock().unwrap().take() {
            remote.close();
        }
    }
}

impl Drop for AlacrittyBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn convert_cursor_shape(shape: VteCursorShape) -> CursorShape {
    match shape {
        VteCursorShape::Block => CursorShape::Block,
        VteCursorShape::Underline => CursorShape::Underline,
        VteCursorShape::Beam => CursorShape::Beam,
        VteCursorShape::HollowBlock => CursorShape::HollowBlock,
        VteCursorShape::Hidden => CursorShape::Hidden,
    }
}

/// Resolve an engine color to concrete RGB, honoring runtime palette
/// overrides (OSC 4) and falling back to the built-in xterm palette.
fn resolve_color(color: Color, colors: &Colors, dim: bool) -> Rgba {
    let mut rgba = match color {
        Color::Spec(rgb) => Rgba::rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => colors[i as usize]
            .map(|rgb| Rgba::rgb(rgb.r, rgb.g, rgb.b))
            .unwrap_or_else(|| indexed_color(i)),
        Color::Named(named) => colors[named]
            .map(|rgb| Rgba::rgb(rgb.r, rgb.g, rgb.b))
            .unwrap_or_else(|| named_color(named)),
    };
    if dim {
        rgba = rgba.dimmed();
    }
    rgba
}

/// Standard xterm defaults for named colors.
fn named_color(named: NamedColor) -> Rgba {
    use NamedColor::*;
    match named {
        Black => Rgba::rgb(0x00, 0x00, 0x00),
        Red => Rgba::rgb(0xcd, 0x00, 0x00),
        Green => Rgba::rgb(0x00, 0xcd, 0x00),
        Yellow => Rgba::rgb(0xcd, 0xcd, 0x00),
        Blue => Rgba::rgb(0x00, 0x00, 0xee),
        Magenta => Rgba::rgb(0xcd, 0x00, 0xcd),
        Cyan => Rgba::rgb(0x00, 0xcd, 0xcd),
        White => Rgba::rgb(0xe5, 0xe5, 0xe5),
        BrightBlack => Rgba::rgb(0x7f, 0x7f, 0x7f),
        BrightRed => Rgba::rgb(0xff, 0x00, 0x00),
        BrightGreen => Rgba::rgb(0x00, 0xff, 0x00),
        BrightYellow => Rgba::rgb(0xff, 0xff, 0x00),
        BrightBlue => Rgba::rgb(0x5c, 0x5c, 0xff),
        BrightMagenta => Rgba::rgb(0xff, 0x00, 0xff),
        BrightCyan => Rgba::rgb(0x00, 0xff, 0xff),
        BrightWhite | BrightForeground => Rgba::rgb(0xff, 0xff, 0xff),
        Foreground => DEFAULT_FG,
        Background => DEFAULT_BG,
        Cursor => DEFAULT_FG,
        DimBlack => Rgba::rgb(0x00, 0x00, 0x00).dimmed(),
        DimRed => Rgba::rgb(0xcd, 0x00, 0x00).dimmed(),
        DimGreen => Rgba::rgb(0x00, 0xcd, 0x00).dimmed(),
        DimYellow => Rgba::rgb(0xcd, 0xcd, 0x00).dimmed(),
        DimBlue => Rgba::rgb(0x00, 0x00, 0xee).dimmed(),
        DimMagenta => Rgba::rgb(0xcd, 0x00, 0xcd).dimmed(),
        DimCyan => Rgba::rgb(0x00, 0xcd, 0xcd).dimmed(),
        DimWhite => Rgba::rgb(0xe5, 0xe5, 0xe5).dimmed(),
        DimForeground => DEFAULT_FG.dimmed(),
    }
}

/// The 256-color palette: 0..16 named, 16..232 color cube, 232..256 grays.
fn indexed_color(i: u8) -> Rgba {
    match i {
        0..=15 => {
            const NAMED: [NamedColor; 16] = [
                NamedColor::Black,
                NamedColor::Red,
                NamedColor::Green,
                NamedColor::Yellow,
                NamedColor::Blue,
                NamedColor::Magenta,
                NamedColor::Cyan,
                NamedColor::White,
                NamedColor::BrightBlack,
                NamedColor::BrightRed,
                NamedColor::BrightGreen,
                NamedColor::BrightYellow,
                NamedColor::BrightBlue,
                NamedColor::BrightMagenta,
                NamedColor::BrightCyan,
                NamedColor::BrightWhite,
            ];
            named_color(NAMED[i as usize])
        }
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            Rgba::rgb(level(i / 36), level((i % 36) / 6), level(i % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Rgba::rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::display_only;

    #[test]
    fn plain_text_and_cursor_position() {
        let (backend, _events) = display_only(20, 5, 100);
        backend.advance(b"hello");
        let snap = backend.snapshot();
        assert_eq!(snap.cols, 20);
        assert_eq!(snap.rows, 5);
        assert!(snap.to_plain_text().starts_with("hello"));
        let cursor = snap.cursor.expect("cursor visible");
        assert_eq!((cursor.col, cursor.row), (5, 0));
        assert_eq!(cursor.shape, CursorShape::Block);
    }

    #[test]
    fn truecolor_and_256_colors() {
        let (backend, _events) = display_only(40, 5, 100);
        backend.advance(b"\x1b[38;5;196mA\x1b[0m\x1b[38;2;12;34;56mB\x1b[0m");
        let snap = backend.snapshot();
        assert_eq!(snap.cell(0, 0).fg, Rgba::rgb(255, 0, 0));
        assert_eq!(snap.cell(1, 0).fg, Rgba::rgb(12, 34, 56));
    }

    #[test]
    fn named_16_colors_and_background() {
        let (backend, _events) = display_only(20, 5, 100);
        backend.advance(b"\x1b[31;44mX\x1b[0m");
        let snap = backend.snapshot();
        assert_eq!(snap.cell(0, 0).fg, Rgba::rgb(0xcd, 0x00, 0x00));
        assert_eq!(snap.cell(0, 0).bg, Rgba::rgb(0x00, 0x00, 0xee));
    }

    #[test]
    fn style_flags() {
        let (backend, _events) = display_only(20, 5, 100);
        backend.advance(b"\x1b[1mB\x1b[3mI\x1b[4mU\x1b[2mD\x1b[0m");
        let snap = backend.snapshot();
        assert!(snap.cell(0, 0).flags.contains(StyleFlags::BOLD));
        assert!(snap.cell(1, 0).flags.contains(StyleFlags::ITALIC));
        assert!(snap.cell(2, 0).flags.contains(StyleFlags::UNDERLINE));
        assert!(snap.cell(3, 0).flags.contains(StyleFlags::DIM));
        // DIM fg is darker than plain.
        backend.advance(b"\x1b[31mR\x1b[2mR\x1b[0m");
        let snap = backend.snapshot();
        assert!(snap.cell(4, 0).fg.r > snap.cell(5, 0).fg.r);
    }

    #[test]
    fn inverse_swaps_fg_bg() {
        let (backend, _events) = display_only(20, 5, 100);
        backend.advance(b"\x1b[31;44mN\x1b[7mI\x1b[0m");
        let snap = backend.snapshot();
        let normal = snap.cell(0, 0);
        let inverse = snap.cell(1, 0);
        assert_eq!((normal.fg, normal.bg), (inverse.bg, inverse.fg));
    }

    #[test]
    fn wide_chars_mark_spacer() {
        let (backend, _events) = display_only(20, 5, 100);
        backend.advance("你x".as_bytes());
        let snap = backend.snapshot();
        assert_eq!(snap.cell(0, 0).c, '你');
        assert!(snap.cell(0, 0).flags.contains(StyleFlags::WIDE_CHAR));
        assert!(snap.cell(1, 0).flags.contains(StyleFlags::WIDE_CHAR_SPACER));
        assert_eq!(snap.cell(2, 0).c, 'x');
        assert_eq!(snap.cursor.unwrap().col, 3);
    }

    #[test]
    fn alt_screen_and_modes() {
        let (backend, _events) = display_only(20, 5, 100);
        assert!(!backend.modes().alt_screen);
        backend.advance(b"\x1b[?1049h");
        assert!(backend.modes().alt_screen);
        assert!(backend.snapshot().alt_screen);
        backend.advance(b"\x1b[?1049l");
        assert!(!backend.modes().alt_screen);
    }

    #[test]
    fn app_cursor_and_bracketed_paste_modes() {
        let (backend, _events) = display_only(20, 5, 100);
        assert!(!backend.modes().app_cursor);
        backend.advance(b"\x1b[?1h");
        assert!(backend.modes().app_cursor);
        backend.advance(b"\x1b[?2004h");
        assert!(backend.modes().bracketed_paste);
    }

    #[test]
    fn scrollback_and_scroll_display() {
        let (backend, _events) = display_only(10, 3, 100);
        for i in 0..10 {
            backend.advance(format!("line{i:02}\r\n").as_bytes());
        }
        let snap = backend.snapshot();
        assert!(snap.history_size > 0);
        assert!(snap.to_plain_text().contains("line09"));
        backend.scroll_display(3);
        let snap = backend.snapshot();
        assert_eq!(snap.display_offset, 3);
        assert!(snap.to_plain_text().contains("line06"));
        backend.scroll_to_bottom();
        assert_eq!(backend.snapshot().display_offset, 0);
    }

    #[test]
    fn resize_reflows_grid() {
        let (backend, _events) = display_only(20, 5, 100);
        backend.advance(b"abcdefghijklmnopqrst");
        backend.resize(10, 5, 8, 16);
        let snap = backend.snapshot();
        assert_eq!(snap.cols, 10);
        // Reflow split the 20-char line in two; the first half moved into
        // scrollback to keep the cursor anchored (nothing is lost).
        assert!(snap.to_plain_text().contains("klmnopqrst"));
        assert!(snap.history_size >= 1);
        backend.scroll_display(1);
        let scrolled = backend.snapshot();
        assert!(scrolled.to_plain_text().contains("abcdefghij"));
        // (Grow-back rejoin semantics across the history boundary are
        // alacritty-internal; real TUIs redraw on SIGWINCH anyway.)
    }

    #[test]
    fn cursor_visibility_and_shapes() {
        let (backend, _events) = display_only(20, 5, 100);
        backend.advance(b"\x1b[?25l"); // hide cursor
        assert!(backend.snapshot().cursor.is_none());
        backend.advance(b"\x1b[?25h\x1b[5 q"); // show + beam
        assert_eq!(backend.snapshot().cursor.unwrap().shape, CursorShape::Beam);
        backend.advance(b"\x1b[3 q"); // underline
        assert_eq!(
            backend.snapshot().cursor.unwrap().shape,
            CursorShape::Underline
        );
    }

    #[test]
    fn split_escape_sequences_across_feeds() {
        let (backend, _events) = display_only(20, 5, 100);
        backend.advance(b"\x1b[38;2;1");
        backend.advance(b"2;34;56mZ\x1b[0m");
        let snap = backend.snapshot();
        assert_eq!(snap.cell(0, 0).c, 'Z');
        assert_eq!(snap.cell(0, 0).fg, Rgba::rgb(12, 34, 56));
    }

    #[test]
    fn combining_chars_attached() {
        let (backend, _events) = display_only(20, 5, 100);
        backend.advance("e\u{301}x".as_bytes());
        let snap = backend.snapshot();
        assert_eq!(snap.cell(0, 0).c, 'e');
        assert_eq!(snap.cell(0, 0).combining.as_deref(), Some(&['\u{301}'][..]));
        assert_eq!(snap.cell(1, 0).c, 'x');
    }
}
