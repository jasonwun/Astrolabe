//! The YAML editor loop (#21): edit → (debounced dry-run precheck) →
//! Apply → jump-to-line on failure → fix → re-Apply. Reused by the Edit-tab
//! and multi-document Create flows built in #16/#17.
//!
//! Marker presentation: gpui-component 0.8.0's Editor has no custom gutter
//! API, so the spec's gutter marks land as line decorations instead —
//! apply errors get a solid (Fill) line tint, precheck results get an
//! outline (Frame) tint. Both move with edits via Monaco-style tracked
//! ranges; details live in the panel / hover-free summary text.

use std::ops::Range;
use std::time::Duration;

use astrolabe_kube::apply::{self, ApplyError, ApplyMode, ApplyReport, Location};
use astrolabe_kube::cluster::{ClusterError, ClusterErrorKind};
use futures::future::BoxFuture;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::language_config::LanguageConfig;
use gpui_kit::component::input::{
    Editor, EditorState, IndentationRules, InputEvent, Position, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle, RopeExt as _, set_language_config,
};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;

use crate::session::Session;

gpui_kit::actions!(
    astrolabe,
    [
        ApplyManifest,
        ForceApplyManifest,
        RunPrecheck,
        TogglePalette
    ]
);

pub const KEY_CONTEXT: &str = "YamlEditor";
const PRECHECK_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Register the edit-time YAML language rules (Enter-indent continuation).
///
/// Independent of the tree-sitter highlight grammar: this is the
/// `input::language_config` type, not the `highlighter` one. Called once at
/// app start; `#2` scoped this at tens of lines.
pub fn register_yaml_language(cx: &mut App) {
    let config = LanguageConfig::default()
        // A mapping line ending in `:` nests the next line one level.
        .indentation_rules(IndentationRules::new(
            regex::Regex::new(r":\s*(#.*)?$").expect("valid regex"),
            // YAML dedents by typing, never by matching text after the
            // caret: a never-matching decrease pattern.
            regex::Regex::new("$.").expect("valid regex"),
        ))
        // YAML has no bracket constructs in block style; keep flow-style
        // pairs but stop quote auto-closing, which misfires on apostrophes.
        .auto_closing_pairs(Vec::new());
    set_language_config("yaml", config, cx);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Error,
    Warning,
}

/// One row of the apply-error panel (levels 1–3 of the degradation chain;
/// level 4 goes to the global error bar instead).
struct PanelItem {
    severity: Severity,
    doc: String,
    field: String,
    message: String,
    line: Option<usize>,
    /// Degradation / anchor annotation, e.g. "nearest existing ancestor,
    /// missing `spec.strategy`" or "value comes from anchor `base`".
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PrecheckStatus {
    Idle,
    Checking,
    /// Background precheck failed (RBAC 403 / network): silent degradation.
    Unavailable,
}

/// The report an Apply or Precheck answers with.
type ApplyAnswer = BoxFuture<'static, Result<ApplyReport, ClusterError>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteCommand {
    Apply,
    Precheck,
}

/// Palette contents: one definition drives both rendering and confirm
/// dispatch. Force Apply is deliberately absent — it is a 409-context panel
/// button (#10), not a global command.
fn palette_entries() -> &'static [(PaletteCommand, &'static str)] {
    &[
        (PaletteCommand::Apply, "Apply (SSA)"),
        (PaletteCommand::Precheck, "Run dry-run precheck now"),
    ]
}

pub struct YamlEditorView {
    state: Entity<EditorState>,
    window_handle: AnyWindowHandle,
    /// Where Apply and Precheck go; with none, the editor says "no
    /// cluster" and sends nothing.
    session: Option<Entity<Session>>,
    /// Bumped when `session` changes, so an Apply or Precheck answered by
    /// the previous Cluster is dropped.
    session_version: u64,
    /// Monotonic buffer version; precheck results older than the current
    /// version are discarded.
    buffer_version: u64,
    /// Version a running precheck was started from.
    precheck_version: Option<u64>,
    precheck_status: PrecheckStatus,
    applying: bool,
    /// The buffer content currently being applied; error mapping runs on it
    /// (#10: the snapshot is what the server saw). If the buffer moved on
    /// while the apply was in flight, causes are re-mapped against the
    /// current buffer best-effort on completion.
    pending_snapshot: Option<String>,
    /// Last apply's panel rows; replaced when the next apply starts,
    /// cleared on success, untouched by edits (#10 lifecycle).
    panel: Vec<PanelItem>,
    /// Managers on the other side of last apply's 409s, shown next to the
    /// panel's Force Apply button.
    conflict_managers: Vec<String>,
    /// Force Apply is offered only in the 409 context (#10: panel button,
    /// never automatic, never default-on).
    force_apply_available: bool,
    /// Global error bar (degradation level 4): full messages, no line jump,
    /// one per failure (a multi-document apply can fail several documents).
    /// Part of the apply result group: replaced when the next apply starts.
    global_notices: Vec<String>,
    /// Solid line tints for apply errors (persist across edits).
    apply_decorations: Option<RangeDecorationCollection>,
    /// Outline line tints for precheck results (cleared on edit / on apply).
    precheck_decorations: Option<RangeDecorationCollection>,
    /// Strict-decoding warning annotation (#15: "unknown field, apply will
    /// ignore it"), shown in the status bar since outline marks are
    /// color-only.
    precheck_note: Option<String>,
    palette: Option<Entity<CommandState>>,
    status_line: String,
}

impl YamlEditorView {
    /// Editor that applies through `session` (#16 Edit YAML tabs), so to
    /// that Session's Cluster.
    pub fn with_session(
        window: &mut Window,
        cx: &mut Context<Self>,
        initial: String,
        session: Entity<Session>,
    ) -> Self {
        let mut view = Self::without_session(window, cx, initial);
        view.set_session(session, cx);
        view
    }

    /// Editor that applies nowhere until [`set_session`](Self::set_session)
    /// gives it a Session.
    pub fn without_session(window: &mut Window, cx: &mut Context<Self>, initial: String) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .default_value(initial)
        });
        state.focus_handle(cx).focus(window, cx);

        cx.subscribe(&state, |this, _state, event: &InputEvent, cx| match event {
            InputEvent::Change => this.on_buffer_changed(cx),
            InputEvent::PressEnter { .. } => this.continue_list_bullet(cx),
            _ => {}
        })
        .detach();

        Self {
            state,
            window_handle: window.window_handle(),
            session: None,
            session_version: 0,
            buffer_version: 0,
            precheck_version: None,
            precheck_status: PrecheckStatus::Idle,
            applying: false,
            pending_snapshot: None,
            panel: Vec::new(),
            conflict_managers: Vec::new(),
            force_apply_available: false,
            global_notices: Vec::new(),
            apply_decorations: None,
            precheck_decorations: None,
            precheck_note: None,
            palette: None,
            status_line: "no cluster".to_string(),
        }
    }

    /// The inner editor entity, exposed for tests and embedders that drive
    /// the editor programmatically.
    pub fn editor_state(&self) -> Entity<EditorState> {
        self.state.clone()
    }

    /// Send Apply and Precheck through `session` from now on. The
    /// Workspace's editor follows the selected Cluster this way (#161).
    pub fn set_session(&mut self, session: Entity<Session>, cx: &mut Context<Self>) {
        if self.session.as_ref() == Some(&session) {
            return;
        }
        // The Session connects lazily and Apply waits on it, so the status
        // names where Apply goes rather than a connection state.
        self.status_line = format!("cluster {}", session.read(cx).state().watch_key().cluster);
        self.session = Some(session);
        self.session_version += 1;
        // The last Apply's result describes the previous Cluster: its Force
        // Apply would overwrite this Cluster's fields.
        self.applying = false;
        self.pending_snapshot = None;
        self.panel.clear();
        self.conflict_managers.clear();
        self.force_apply_available = false;
        self.global_notices.clear();
        self.clear_apply_decorations(cx);
        // So does the last Precheck's; the next edit checks against this one.
        self.clear_precheck(cx);
        cx.notify();
    }

    // ------------------------------------------------------------ cluster I/O

    /// Send `request` through the Session; `None` when there is none.
    fn send(
        &self,
        request: impl FnOnce(&mut Session) -> ApplyAnswer,
        cx: &mut Context<Self>,
    ) -> Option<ApplyAnswer> {
        let session = self.session.as_ref()?;
        Some(session.update(cx, |session, _| request(session)))
    }

    fn apply(&mut self, mode: ApplyMode, cx: &mut Context<Self>) {
        if self.applying {
            return;
        }
        let snapshot = self.state.read(cx).value().to_string();
        let Some(applied) = self.send(|session| session.apply(&snapshot, mode), cx) else {
            self.global_notices = vec![no_cluster_notice("apply")];
            cx.notify();
            return;
        };
        self.applying = true;
        self.pending_snapshot = Some(snapshot.clone());
        // Next apply replaces the whole previous result group — panel, line
        // marks, and the global error bar alike (#10 lifecycle).
        self.panel.clear();
        self.conflict_managers.clear();
        self.force_apply_available = false;
        self.global_notices.clear();
        self.clear_apply_decorations(cx);
        self.status_line = match mode {
            ApplyMode::Normal => "applying…".to_string(),
            ApplyMode::Force => "force applying…".to_string(),
        };
        cx.notify();

        let session_version = self.session_version;
        cx.spawn(async move |view, cx| {
            let result = applied.await;
            view.update(cx, |this, cx| {
                if this.session_version == session_version {
                    this.finish_apply(result, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn finish_apply(&mut self, result: Result<ApplyReport, ClusterError>, cx: &mut Context<Self>) {
        self.applying = false;
        // The server gave an authoritative answer; precheck rehearsal is
        // void regardless of outcome (#15).
        self.clear_precheck(cx);

        let mut report = match result {
            Ok(report) => report,
            Err(e) => {
                self.pending_snapshot = None;
                self.global_notices = vec![failure_notice("apply", &e)];
                self.status_line = "apply failed".to_string();
                cx.notify();
                return;
            }
        };
        // The buffer may have moved on while the apply was in flight:
        // re-map every cause against the current buffer, best-effort (#10).
        if let Some(snapshot) = self.pending_snapshot.take()
            && self.state.read(cx).value().as_ref() != snapshot.as_str()
        {
            let current = self.state.read(cx).value().to_string();
            for outcome in &mut report.outcomes {
                if let Err(error) = &mut outcome.result {
                    for located in &mut error.causes {
                        located.located =
                            apply::locate_field(&current, outcome.doc_index, &located.cause.path);
                    }
                }
            }
        }
        if report.ok {
            self.status_line = "applied".to_string();
            cx.notify();
            return;
        }

        let mut errors = 0;
        for outcome in &report.outcomes {
            let Err(error) = &outcome.result else {
                continue;
            };
            match &error.error {
                ApplyError::Global { message } => {
                    self.global_notices
                        .push(format!("{}: {message}", outcome.doc));
                }
                ApplyError::Validation(_) => {
                    errors += error.causes.len();
                    for located in &error.causes {
                        self.panel
                            .push(panel_item(outcome, &located.cause, &located.located));
                    }
                }
                ApplyError::Conflict(conflicts) => {
                    errors += error.causes.len();
                    self.force_apply_available = true;
                    for conflict in conflicts {
                        if !self.conflict_managers.contains(&conflict.manager) {
                            self.conflict_managers.push(conflict.manager.clone());
                        }
                    }
                    if conflicts.is_empty() {
                        // Message failed to parse: degrade to the global bar.
                        self.global_notices.push(format!(
                            "{}: apply conflicted (unparsable details); force apply available",
                            outcome.doc
                        ));
                    }
                    for located in &error.causes {
                        self.panel
                            .push(panel_item(outcome, &located.cause, &located.located));
                    }
                }
            }
        }
        self.status_line = format!("apply failed: {errors} problem(s)");
        self.rebuild_apply_decorations(cx);
        cx.notify();
    }

    // ------------------------------------------------------------ precheck

    fn on_buffer_changed(&mut self, cx: &mut Context<Self>) {
        self.buffer_version += 1;
        let version = self.buffer_version;
        // Precheck results describe the buffer they checked; any edit voids
        // them, and the debounce re-checks (#15 live-diagnostics semantics).
        self.clear_precheck(cx);
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(PRECHECK_DEBOUNCE).await;
            view.update(cx, |this, cx| {
                if this.buffer_version == version && !this.applying {
                    this.run_precheck(false, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn run_precheck(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.state.read(cx).value().trim().is_empty() {
            return;
        }
        let snapshot = self.state.read(cx).value().to_string();
        let Some(checked) = self.send(|session| session.precheck(&snapshot), cx) else {
            // The debounced Precheck stays silent: the status already says
            // "no cluster".
            if manual {
                self.global_notices = vec![no_cluster_notice("precheck")];
                cx.notify();
            }
            return;
        };
        let version = self.buffer_version;
        self.precheck_version = Some(version);
        self.precheck_status = PrecheckStatus::Checking;
        cx.notify();

        let session_version = self.session_version;
        cx.spawn(async move |view, cx| {
            let result = checked.await;
            view.update(cx, |this, cx| {
                if this.session_version == session_version {
                    this.finish_precheck(version, manual, result, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn finish_precheck(
        &mut self,
        version: u64,
        manual: bool,
        result: Result<ApplyReport, ClusterError>,
        cx: &mut Context<Self>,
    ) {
        self.precheck_status = PrecheckStatus::Idle;
        // Stale result: the buffer moved on; the debounce re-checks.
        if self.buffer_version != version {
            return;
        }
        let report = match result {
            Ok(report) => report,
            Err(e) => {
                if manual {
                    self.global_notices = vec![failure_notice("precheck", &e)];
                } else {
                    self.precheck_status = PrecheckStatus::Unavailable;
                }
                cx.notify();
                return;
            }
        };
        let mut unavailable: Vec<String> = Vec::new();
        let mut warnings = 0usize;
        let mut marks: Vec<(usize, Severity)> = Vec::new();
        for outcome in &report.outcomes {
            let Err(error) = &outcome.result else {
                continue;
            };
            if let ApplyError::Global { message } = &error.error {
                unavailable.push(format!("{}: {message}", outcome.doc));
                continue;
            }
            for located in &error.causes {
                let Some(line) = located_line(&located.located) else {
                    continue;
                };
                // Strict-decoding causes (unknown/duplicate field) render
                // as warnings: a real apply ignores those fields (#15).
                let severity = if located.cause.is_strict_decoding() {
                    Severity::Warning
                } else {
                    Severity::Error
                };
                if severity == Severity::Warning {
                    warnings += 1;
                }
                marks.push((line, severity));
            }
        }
        // The outline marks are color-only, so the warning annotation lives
        // in the status bar (#15: note that apply will ignore the field).
        self.precheck_note = (warnings > 0)
            .then(|| format!("{warnings} unknown/duplicate field(s) — apply will ignore them"));
        let red = cx.theme().red;
        let yellow = cx.theme().yellow;
        let collection = self.state.update(cx, |state, cx| {
            let decorations = marks
                .into_iter()
                .filter_map(|(line, severity)| {
                    let color = match severity {
                        Severity::Error => red,
                        Severity::Warning => yellow,
                    };
                    line_byte_range(state, line).map(|range| {
                        RangeDecoration::new(range)
                            .with_style(RangeDecorationStyle::Frame)
                            .with_color(color)
                    })
                })
                .collect();
            state.create_range_decorations_collection(decorations, cx)
        });
        if let Some(old) = self.precheck_decorations.replace(collection) {
            old.dispose(cx);
        }
        if !unavailable.is_empty() && manual {
            self.global_notices = unavailable;
        } else if !unavailable.is_empty() {
            self.precheck_status = PrecheckStatus::Unavailable;
        }
        cx.notify();
    }

    fn clear_precheck(&mut self, cx: &mut Context<Self>) {
        self.precheck_version = None;
        if let Some(collection) = self.precheck_decorations.take() {
            collection.dispose(cx);
        }
        self.precheck_status = PrecheckStatus::Idle;
        self.precheck_note = None;
    }

    // ------------------------------------------------------------ decorations

    fn rebuild_apply_decorations(&mut self, cx: &mut Context<Self>) {
        let red = cx.theme().red;
        let mut fill = red;
        fill.a = 0.18;
        let lines: Vec<usize> = self.panel.iter().filter_map(|item| item.line).collect();
        let collection = self.state.update(cx, |state, cx| {
            let decorations = lines
                .into_iter()
                .filter_map(|line| line_byte_range(state, line))
                .map(|range| {
                    RangeDecoration::new(range)
                        .with_style(RangeDecorationStyle::Fill)
                        .with_color(fill)
                })
                .collect();
            state.create_range_decorations_collection(decorations, cx)
        });
        self.apply_decorations = Some(collection);
    }

    fn clear_apply_decorations(&mut self, cx: &mut Context<Self>) {
        if let Some(collection) = self.apply_decorations.take() {
            collection.dispose(cx);
        }
    }

    // ------------------------------------------------------------ editing

    /// `- ` list-item continuation (#6's known gap, patched without forking
    /// the editor): after Enter, if the previous line was a list item and
    /// the new line holds only auto-indent whitespace, insert the bullet.
    fn continue_list_bullet(&mut self, cx: &mut Context<Self>) {
        let needs_bullet = self.state.read_with(cx, |state, _cx| {
            let cursor: Position = state.cursor_position();
            let row = cursor.line as usize;
            if row == 0 {
                return false;
            }
            let rope = state.text();
            let current = rope.slice_line(row).to_string();
            if !current.trim().is_empty() {
                return false;
            }
            let previous = rope.slice_line(row - 1).to_string();
            let trimmed = previous.trim_start();
            trimmed.starts_with("- ") && trimmed.trim() != "-"
        });
        if !needs_bullet {
            return;
        }
        self.window_handle
            .update(cx, |_, window, cx| {
                self.state
                    .update(cx, |state, cx| state.insert("- ", window, cx));
            })
            .ok();
    }

    fn jump_to(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let position = Position::new(line.saturating_sub(1) as u32, 0);
            state.unfold_at(position, cx);
            state.set_cursor_position(position, window, cx);
        });
    }

    // ------------------------------------------------------------ actions

    fn on_apply(&mut self, _: &ApplyManifest, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply(ApplyMode::Normal, cx);
    }

    fn on_force_apply(
        &mut self,
        _: &ForceApplyManifest,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply(ApplyMode::Force, cx);
    }

    fn on_precheck(&mut self, _: &RunPrecheck, _window: &mut Window, cx: &mut Context<Self>) {
        self.run_precheck(true, cx);
    }

    fn on_toggle_palette(
        &mut self,
        _: &TogglePalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.take().is_none() {
            let palette = cx.new(|cx| CommandState::new(window, cx));
            palette.focus_handle(cx).focus(window, cx);
            self.palette = Some(palette);
        }
        cx.notify();
    }

    fn on_palette_confirm(
        &mut self,
        index: &gpui_kit::component::IndexPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette = None;
        self.state.focus_handle(cx).focus(window, cx);
        match palette_entries().get(index.row).map(|(command, _)| command) {
            Some(PaletteCommand::Apply) => self.apply(ApplyMode::Normal, cx),
            Some(PaletteCommand::Precheck) => self.run_precheck(true, cx),
            None => {}
        }
        cx.notify();
    }

    // ------------------------------------------------------------ rendering

    fn render_panel(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.panel.is_empty() && self.conflict_managers.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let mut header = format!("{} problem(s) from last apply", self.panel.len());
        if !self.conflict_managers.is_empty() {
            header.push_str(&format!(
                " — conflicts with {}",
                self.conflict_managers.join(", ")
            ));
        }

        let rows = self
            .panel
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let note = item
                    .note
                    .as_ref()
                    .map(|n| format!(" — {n}"))
                    .unwrap_or_default();
                let text = format!(
                    "{} [{}] {}: {}{}",
                    match item.severity {
                        Severity::Error => "error",
                        Severity::Warning => "warning",
                    },
                    item.doc,
                    item.field,
                    item.message,
                    note
                );
                let mut row = div()
                    .id(i)
                    .px_2()
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(text);
                if let Some(line) = item.line {
                    row = row
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.jump_to(line, window, cx);
                        }));
                } else {
                    row = row.text_color(theme.muted_foreground);
                }
                row
            })
            .collect::<Vec<_>>();

        let mut header_row = div().px_2().py_1().flex().justify_between().child(
            div()
                .id("yaml-problems")
                .aria_label(header.clone())
                .test_support()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(header),
        );
        if self.force_apply_available {
            header_row = header_row.child(
                div()
                    .id("force-apply")
                    .aria_label("Force Apply")
                    .test_support()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme.red)
                    .child("Force Apply")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_force_apply(&ForceApplyManifest, window, cx);
                    })),
            );
        }

        let mut panel = v_flex()
            .w_full()
            .max_h(px(180.))
            .border_t_1()
            .border_color(theme.border)
            .child(header_row);
        for row in rows {
            panel = panel.child(row);
        }
        Some(panel)
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let precheck = match &self.precheck_status {
            PrecheckStatus::Idle => self.precheck_note.clone().unwrap_or_default(),
            PrecheckStatus::Checking => "checking…".to_string(),
            PrecheckStatus::Unavailable => "● precheck unavailable".to_string(),
        };
        div()
            .w_full()
            .px_2()
            .py_1()
            .flex()
            .justify_between()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("yaml-status")
                    .aria_label(self.status_line.clone())
                    .test_support()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(self.status_line.clone()),
            )
            .child(
                div()
                    .id("yaml-precheck")
                    .aria_label(precheck.clone())
                    .test_support()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(precheck),
            )
    }

    fn render_palette(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let palette = self.palette.as_ref()?;
        let view = cx.entity().downgrade();
        let cancel_view = cx.entity().downgrade();
        let mut group = CommandGroup::new().label("YAML editor");
        for (_, label) in palette_entries() {
            group = group.item(CommandItem::new().label(*label));
        }
        Some(
            div()
                .absolute()
                .top(px(48.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div().w(px(420.)).child(
                        Command::new(palette)
                            .placeholder("YAML editor command…")
                            .group(group)
                            .on_confirm(move |index, _window, cx| {
                                view.update_in(cx, |this, window, cx| {
                                    this.on_palette_confirm(&index, window, cx);
                                })
                                .ok();
                            })
                            .on_cancel(move |_window, cx| {
                                cancel_view
                                    .update_in(cx, |this, window, cx| {
                                        this.palette = None;
                                        this.state.focus_handle(cx).focus(window, cx);
                                        cx.notify();
                                    })
                                    .ok();
                            }),
                    ),
                ),
        )
    }
}

impl Focusable for YamlEditorView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.focus_handle(cx)
    }
}

impl Render for YamlEditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut root = v_flex()
            .relative()
            .size_full()
            .bg(theme.background)
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::on_apply))
            .on_action(cx.listener(Self::on_force_apply))
            .on_action(cx.listener(Self::on_precheck))
            .on_action(cx.listener(Self::on_toggle_palette));
        if !self.global_notices.is_empty() {
            let notice = self.global_notices.join("\n");
            root = root.child(
                div()
                    .id("global-notice")
                    .aria_label(notice.clone())
                    .test_support()
                    .w_full()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme.red)
                    .border_b_1()
                    .border_color(theme.border)
                    .children(
                        self.global_notices
                            .iter()
                            .map(|line| div().child(line.clone())),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.global_notices.clear();
                        cx.notify();
                    })),
            );
        }
        root = root.child(
            div()
                .flex_1()
                .min_h_0()
                .child(Editor::new(&self.state).h_full()),
        );
        if let Some(panel) = self.render_panel(cx) {
            root = root.child(panel);
        }
        root = root.child(self.render_status_bar(cx));
        if let Some(palette) = self.render_palette(cx) {
            root = root.child(palette);
        }
        root
    }
}

// ---------------------------------------------------------------- helpers

/// The notice for a `verb` ("apply", "precheck") that had no Cluster to go
/// to.
fn no_cluster_notice(verb: &str) -> String {
    format!("cannot {verb}: no cluster")
}

/// The notice for a `verb` ("apply", "precheck") the Cluster failed.
fn failure_notice(verb: &str, error: &ClusterError) -> String {
    match error.kind() {
        ClusterErrorKind::Connect => format!("cannot {verb}: cluster unreachable ({error})"),
        _ => error.to_string(),
    }
}

fn located_line(located: &apply::Located) -> Option<usize> {
    match located.location {
        Location::Exact { line } => Some(line),
        Location::Ancestor { line, .. } => Some(line),
        Location::DocHeader { line } => Some(line),
        Location::Global => None,
    }
}

fn panel_item(
    outcome: &apply::DocOutcome,
    cause: &apply::FieldCause,
    located: &apply::Located,
) -> PanelItem {
    let note = match &located.location {
        Location::Ancestor { missing, .. } => Some(format!(
            "nearest existing ancestor; `{}` not found in manifest",
            missing
        )),
        Location::DocHeader { .. } => {
            Some("cannot locate precisely; at document start".to_string())
        }
        _ => located
            .anchor_note
            .as_ref()
            .map(|a| format!("value comes from anchor `{a}`")),
    };
    PanelItem {
        severity: Severity::Error,
        doc: outcome.doc.clone(),
        field: cause.raw_field.clone(),
        message: cause.message.clone(),
        line: located_line(located),
        note,
    }
}

/// 1-based absolute line → UTF-8 byte range covering the line's content.
fn line_byte_range(state: &EditorState, line: usize) -> Option<Range<usize>> {
    if line == 0 {
        return None;
    }
    let rope = state.text();
    let row = line - 1;
    if row >= rope.lines_len() {
        return None;
    }
    let start = rope.line_start_offset(row);
    let mut end = rope.line_end_offset(row);
    if end == start {
        end = (start + 1).min(rope.len());
    }
    (end > start).then_some(start..end)
}
