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

use astrolabe_kube::apply::{ApplyMode, ApplyReport};
use astrolabe_kube::cluster::ClusterError;
use futures::future::BoxFuture;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::language_config::LanguageConfig;
use gpui_kit::component::input::{
    Editor, EditorState, IndentationRules, InputEvent, Position, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle, RopeExt as _, set_language_config,
};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;

use astrolabe_model::edit_loop::{EditLoop, Panel, Severity};

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
    /// The Edit loop's rules and what they show.
    edit: EditLoop,
    /// Solid line tints for apply errors (persist across edits), drawn for
    /// these lines.
    apply_decorations: Option<(Vec<usize>, RangeDecorationCollection)>,
    /// Outline line tints for precheck results, drawn for these marks.
    precheck_decorations: Option<(Vec<(usize, Severity)>, RangeDecorationCollection)>,
    palette: Option<Entity<CommandState>>,
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
            edit: EditLoop::new(),
            apply_decorations: None,
            precheck_decorations: None,
            palette: None,
        }
    }

    /// The inner editor entity, exposed for tests and embedders that drive
    /// the editor programmatically.
    pub fn editor_state(&self) -> Entity<EditorState> {
        self.state.clone()
    }

    /// The byte ranges of the lines the last Apply's errors tint.
    pub fn apply_mark_ranges(&self, cx: &App) -> Vec<Range<usize>> {
        self.apply_decorations
            .as_ref()
            .map(|(_, collection)| collection.get_ranges(cx))
            .unwrap_or_default()
    }

    /// Send Apply and Precheck through `session` from now on. The
    /// Workspace's editor follows the selected Cluster this way (#161).
    pub fn set_session(&mut self, session: Entity<Session>, cx: &mut Context<Self>) {
        if self.session.as_ref() == Some(&session) {
            return;
        }
        let cluster = session.read(cx).state().watch_key().cluster.to_string();
        self.session = Some(session);
        self.edit.cluster_changed(&cluster);
        self.sync_decorations(cx);
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
        let buffer = self.state.read(cx).value().to_string();
        let ticket = self.edit.begin_apply(&buffer, mode);
        let answer = ticket.and_then(|_| self.send(|session| session.apply(&buffer, mode), cx));
        self.sync_decorations(cx);
        cx.notify();
        let (Some(ticket), Some(applied)) = (ticket, answer) else {
            return;
        };
        cx.spawn(async move |view, cx| {
            let result = applied.await;
            view.update(cx, |this, cx| {
                let current = this.state.read(cx).value().to_string();
                this.edit.finish_apply(ticket, result, &current);
                this.sync_decorations(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ------------------------------------------------------------ precheck

    fn on_buffer_changed(&mut self, cx: &mut Context<Self>) {
        let version = self.edit.buffer_changed();
        self.sync_decorations(cx);
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(PRECHECK_DEBOUNCE).await;
            view.update(cx, |this, cx| {
                if this.edit.precheck_due(version) {
                    this.run_precheck(false, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn run_precheck(&mut self, manual: bool, cx: &mut Context<Self>) {
        let buffer = self.state.read(cx).value().to_string();
        let ticket = self.edit.begin_precheck(&buffer, manual);
        let answer = ticket.and_then(|_| self.send(|session| session.precheck(&buffer), cx));
        cx.notify();
        let (Some(ticket), Some(checked)) = (ticket, answer) else {
            return;
        };
        cx.spawn(async move |view, cx| {
            let result = checked.await;
            view.update(cx, |this, cx| {
                this.edit.finish_precheck(ticket, manual, result);
                this.sync_decorations(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ------------------------------------------------------------ decorations

    /// Make the tints match what the Edit loop shows. A tint is only
    /// rebuilt when its lines changed: it follows edits on its own.
    fn sync_decorations(&mut self, cx: &mut Context<Self>) {
        let shown = self.edit.projection();
        let red = cx.theme().red;
        let yellow = cx.theme().yellow;

        let drawn = self.precheck_decorations.as_ref().map(|(marks, _)| marks);
        if drawn.map_or(!shown.precheck_marks.is_empty(), |d| {
            *d != shown.precheck_marks
        }) {
            if let Some((_, old)) = self.precheck_decorations.take() {
                old.dispose(cx);
            }
            if !shown.precheck_marks.is_empty() {
                let marks = shown.precheck_marks;
                let collection = self.state.update(cx, |state, cx| {
                    let decorations = marks
                        .iter()
                        .filter_map(|&(line, severity)| {
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
                self.precheck_decorations = Some((marks, collection));
            }
        }

        let drawn = self.apply_decorations.as_ref().map(|(lines, _)| lines);
        if drawn.map_or(!shown.apply_lines.is_empty(), |d| *d != shown.apply_lines) {
            if let Some((_, old)) = self.apply_decorations.take() {
                old.dispose(cx);
            }
            if !shown.apply_lines.is_empty() {
                let lines = shown.apply_lines;
                let mut fill = red;
                fill.a = 0.18;
                let collection = self.state.update(cx, |state, cx| {
                    let decorations = lines
                        .iter()
                        .filter_map(|&line| line_byte_range(state, line))
                        .map(|range| {
                            RangeDecoration::new(range)
                                .with_style(RangeDecorationStyle::Fill)
                                .with_color(fill)
                        })
                        .collect();
                    state.create_range_decorations_collection(decorations, cx)
                });
                self.apply_decorations = Some((lines, collection));
            }
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

    fn render_panel(&mut self, panel: Panel, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let rows = panel
            .rows
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let mut row = div()
                    .id(i)
                    .px_2()
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(item.text);
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
                .aria_label(panel.header.clone())
                .test_support()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(panel.header),
        );
        if panel.force_apply {
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
        panel
    }

    fn render_status_bar(
        &self,
        status: String,
        precheck: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
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
                    .aria_label(status.clone())
                    .test_support()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(status),
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
        let shown = self.edit.projection();
        if !shown.notices.is_empty() {
            let notice = shown.notices.join("\n");
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
                    .children(shown.notices.into_iter().map(|line| div().child(line)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit.dismiss_notices();
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
        if let Some(panel) = shown.panel {
            root = root.child(self.render_panel(panel, cx));
        }
        root = root.child(self.render_status_bar(shown.status, shown.precheck, cx));
        if let Some(palette) = self.render_palette(cx) {
            root = root.child(palette);
        }
        root
    }
}

// ---------------------------------------------------------------- helpers

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
