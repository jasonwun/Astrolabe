//! Engine boundary for the terminal.
//!
//! Everything the UI knows about a terminal session is defined here as
//! crate-owned types. The VT engine (currently `alacritty_terminal`) is
//! confined to [`alacritty`]; no engine type appears in a public signature
//! of this module or anywhere outside it, so a different engine can
//! replace the implementation without touching view or input code.

pub mod alacritty;
pub mod keys;

pub use alacritty::{AlacrittyBackend, EventStream};

/// Configuration for one terminal session.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Program to run in the PTY (e.g. the user's shell).
    pub program: String,
    /// Arguments for the program.
    pub args: Vec<String>,
    /// Extra environment variables (TERM, COLORTERM, …).
    pub env: Vec<(String, String)>,
    /// Initial grid width in cells.
    pub cols: usize,
    /// Initial grid height in cells.
    pub rows: usize,
    /// Cell width at spawn time (informational for the PTY).
    pub cell_width: u16,
    /// Cell height at spawn time (informational for the PTY).
    pub cell_height: u16,
    /// Maximum scrollback lines.
    pub scrollback: usize,
}

/// The session interface: everything the UI can ask of a terminal.
///
/// All methods take `&self`; implementations synchronize internally so the
/// GPUI main thread never holds an engine lock across a paint.
pub trait TerminalBackend: Send + Sync + 'static {
    /// Bytes from the user (keyboard, paste, application replies) → child.
    fn write_input(&self, bytes: &[u8]);

    /// Grid size changed (the caller recomputes cells from pixels).
    /// Must propagate to both the engine grid and the PTY (SIGWINCH).
    fn resize(&self, cols: usize, rows: usize, cell_width: u16, cell_height: u16);

    /// Scroll the *display* into scrollback; positive = up. Does not
    /// affect the child process.
    fn scroll_display(&self, delta_lines: i32);

    /// Jump the display back to the live edge.
    fn scroll_to_bottom(&self);

    /// Copy out a render-ready snapshot of the visible grid.
    fn snapshot(&self) -> RenderSnapshot;

    /// Feed an ANSI output byte stream into the terminal. Only meaningful
    /// for display-only (no-PTY) backends — the log-stream use case
    /// (`kubectl logs -f`); PTY sessions receive output from the child.
    /// Default: no-op.
    fn feed_output(&self, _bytes: &[u8]) {}

    /// Current terminal modes relevant to input encoding and rendering.
    fn modes(&self) -> TerminalModes;

    /// Answer an application's clipboard query (OSC 52 load) with `text`.
    /// Default: ignore.
    fn respond_clipboard_load(&self, _text: &str) {}

    /// Counters for wakeup-coalescing and damage observations.
    fn stats(&self) -> BackendStats {
        BackendStats::default()
    }

    /// End the session: stop IO threads, kill the child. Idempotent.
    fn shutdown(&self);
}

/// The far end of a remote session (`kubectl exec`): it owns the process,
/// receives the user's input and grid size, and sends output back through
/// [`TerminalBackend::feed_output`].
pub trait RemoteSink: Send + Sync + 'static {
    /// Bytes from the user → the remote process's stdin.
    fn input(&self, bytes: &[u8]);
    /// The grid size changed (the remote TTY's window size).
    fn resize(&self, cols: u16, rows: u16);
    /// The session ended on this side; called once.
    fn close(&self);
}

/// Events flowing from the engine/session to the UI.
///
/// `Wakeup` is expected to arrive in bursts; the UI must coalesce (batch
/// drain, single notify per batch) rather than notify per event.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// Grid content changed; re-render.
    Wakeup,
    /// Window/tab title changed (OSC 0/1/2).
    Title(String),
    /// Title reset to default.
    ResetTitle,
    /// BEL.
    Bell,
    /// The application asked to write bytes to its own stdin
    /// (e.g. device-attributes replies); forward verbatim to the PTY.
    PtyWrite(Vec<u8>),
    /// Application wants to store text in a clipboard (OSC 52).
    ClipboardStore {
        clipboard: ClipboardKind,
        text: String,
    },
    /// Application wants to read a clipboard (OSC 52 load). Answer via
    /// [`TerminalBackend::respond_clipboard_load`].
    ClipboardLoad { clipboard: ClipboardKind },
    /// Application queried a color (OSC 10/11/12); currently unanswered.
    ColorRequest { index: usize },
    /// The child process exited.
    ChildExit(ExitStatus),
    /// The engine asked the session to shut down (e.g. `exit` ioctl path).
    Exit,
    /// Cursor blink state or mouse cursor shape may have changed.
    CursorBlinkingChange,
    /// Grid change may affect the mouse cursor shape (hyperlink hover).
    MouseCursorDirty,
}

/// Which clipboard an OSC 52 targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardKind {
    Clipboard,
    /// Primary selection (X11); on macOS treat as Clipboard.
    Selection,
}

/// Engine-independent child exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: Option<i32>,
    /// Signal that killed the child, if any (unix).
    pub signal: Option<i32>,
}

/// Terminal modes the UI needs for input encoding and rendering.
#[derive(Debug, Clone, Copy, Default)]
pub struct TerminalModes {
    pub alt_screen: bool,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub show_cursor: bool,
    /// Any mouse reporting is active (affects wheel behavior).
    pub mouse_report: bool,
    /// SGR (1006) mouse encoding requested.
    pub sgr_mouse: bool,
    pub line_wrap: bool,
}

/// Default foreground (no theme system yet).
pub const DEFAULT_FG: Rgba = Rgba::rgb(0xd8, 0xd8, 0xd8);
/// Default background (no theme system yet).
pub const DEFAULT_BG: Rgba = Rgba::rgb(0x18, 0x18, 0x18);

/// Straight-alpha RGB color, already resolved (palette/inverse applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 0xff }
    }

    pub fn dimmed(self) -> Self {
        let d = |v: u8| ((v as u16 * 2) / 3) as u8;
        Self::rgb(d(self.r), d(self.g), d(self.b))
    }
}

/// Style attributes of one cell. `INVERSE` is *not* here: it is resolved
/// (fg/bg swapped) at snapshot time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StyleFlags(u16);

impl StyleFlags {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1 << 0);
    pub const ITALIC: Self = Self(1 << 1);
    pub const DIM: Self = Self(1 << 2);
    pub const UNDERLINE: Self = Self(1 << 3);
    pub const DOUBLE_UNDERLINE: Self = Self(1 << 4);
    pub const UNDERCURL: Self = Self(1 << 5);
    pub const STRIKEOUT: Self = Self(1 << 6);
    /// First cell of a double-width glyph (CJK, most emoji).
    pub const WIDE_CHAR: Self = Self(1 << 7);
    /// Continuation cell of a double-width glyph; render nothing here.
    pub const WIDE_CHAR_SPACER: Self = Self(1 << 8);
    /// Logical line continues on the next row (soft wrap).
    pub const WRAPLINE: Self = Self(1 << 9);
    /// Cell glyph is invisible (conceal).
    pub const HIDDEN: Self = Self(1 << 10);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for StyleFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// One grid cell in a [`RenderSnapshot`].
#[derive(Debug, Clone)]
pub struct SnapshotCell {
    pub c: char,
    /// Zero-width combining characters attached to this cell.
    pub combining: Option<Vec<char>>,
    pub fg: Rgba,
    pub bg: Rgba,
    pub flags: StyleFlags,
}

/// Cursor shapes, mirroring DECSCUSR plus hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Beam,
    HollowBlock,
    Hidden,
}

/// Cursor position within the visible grid.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotCursor {
    pub col: usize,
    pub row: usize,
    pub shape: CursorShape,
}

/// A copy of everything needed to paint one frame.
#[derive(Debug, Clone)]
pub struct RenderSnapshot {
    pub cols: usize,
    pub rows: usize,
    /// Row-major, `cols * rows` entries, including blank padding.
    pub cells: Vec<SnapshotCell>,
    /// `None` when the cursor is hidden or scrolled out of view.
    pub cursor: Option<SnapshotCursor>,
    /// Lines the display is scrolled back (0 = live edge).
    pub display_offset: usize,
    /// Current scrollback size in lines.
    pub history_size: usize,
    pub alt_screen: bool,
}

impl RenderSnapshot {
    pub fn cell(&self, col: usize, row: usize) -> &SnapshotCell {
        &self.cells[row * self.cols + col]
    }

    /// Plain-text rendering of the visible grid (for smoke tests/dumps).
    /// Wide-char spacer cells are skipped, so text matches what a human
    /// reads; column positions of wide glyphs are not preserved.
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for row in 0..self.rows {
            let line: String = (0..self.cols)
                .filter(|&c| {
                    !self
                        .cell(c, row)
                        .flags
                        .contains(StyleFlags::WIDE_CHAR_SPACER)
                })
                .map(|c| self.cell(c, row).c)
                .collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.to_plain_text().contains(needle)
    }
}

/// Wakeup/damage counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackendStats {
    /// Wakeups delivered by the engine since session start.
    pub wakeups: u64,
    /// Snapshots where the engine reported full-grid damage.
    pub damage_full: u64,
    /// Damaged lines across partial-damage snapshots.
    pub damage_partial_lines: u64,
    /// Snapshots taken.
    pub snapshots: u64,
}

/// Spawn an interactive shell session: alacritty's own event loop drives
/// PTY IO; replaceable by hand-written threads behind this same signature.
pub fn spawn_shell(config: &SessionConfig) -> (AlacrittyBackend, EventStream) {
    AlacrittyBackend::spawn(config)
}

/// Create a display-only terminal (no PTY): feed ANSI byte streams via
/// [`TerminalBackend::feed_output`]. This is the log-stream use case
/// (kubectl logs) and the unit-test harness.
pub fn display_only(
    cols: usize,
    rows: usize,
    scrollback: usize,
) -> (AlacrittyBackend, EventStream) {
    AlacrittyBackend::display_only(cols, rows, scrollback)
}

/// Create a remote terminal: rendered like [`display_only`], with input
/// and resizes going to `sink`. Report the far end's exit through
/// [`AlacrittyBackend::remote_exited`].
pub fn remote(
    cols: usize,
    rows: usize,
    scrollback: usize,
    sink: Box<dyn RemoteSink>,
) -> (AlacrittyBackend, EventStream) {
    AlacrittyBackend::remote(cols, rows, scrollback, sink)
}
