//! The Edit loop's rules (#299): a YAML buffer's path from editing to
//! Apply, and the Precheck and Apply results that round leaves.
//!
//! [`EditLoop`] is driven by the editor view. The view asks to begin an
//! Apply or Precheck and gets a [`Ticket`] to send the request with, hands
//! the answer back with that ticket, and draws [`EditLoop::projection`].
//! Tickets from a previous Cluster or an older buffer are ignored, so the
//! view compares no versions. The debounce timer, the editor and the
//! request itself stay in the view.

use std::fmt::Write as _;

use astrolabe_kube::apply::{
    ApplyError, ApplyMode, ApplyReport, DocOutcome, FieldCause, Located, Location,
};
use astrolabe_kube::cluster::{ClusterError, ClusterErrorKind};

use crate::notice::{Intent, Notice};

/// How a Precheck mark is drawn: an error, or a warning for a field a real
/// Apply would ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// What a request in flight was started from. A request answered with a
/// ticket that is no longer current is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    cluster_version: u64,
    buffer_version: u64,
    /// How many Applies had started; a Precheck answered after another
    /// Apply started is void.
    apply_count: u64,
}

/// The buffer version an edit produced, for the debounced Precheck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferVersion(u64);

/// One row of the problem panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemRow {
    pub text: String,
    /// The 1-based buffer line a click jumps to; a row without one is
    /// drawn muted.
    pub line: Option<usize>,
}

/// The problem panel: shown while the last Apply left problems or named
/// the managers it conflicted with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemPanel {
    pub header: String,
    pub rows: Vec<ProblemRow>,
    /// Whether the Force Apply button is offered (a 409 context).
    pub force_apply: bool,
}

/// Everything the editor draws, as final values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    /// The global notice, one line per text: the Apply's, then the manual
    /// Precheck's. A click dismisses them all. None when there is none.
    pub notice: Option<Notice>,
    pub panel: Option<ProblemPanel>,
    pub status: Notice,
    /// The Precheck text of the status bar: empty when there is nothing to
    /// say.
    pub precheck: Notice,
    /// `(1-based line, severity)` of the Precheck's outline marks.
    pub precheck_marks: Vec<(usize, Severity)>,
    /// The 1-based lines of the last Apply's errors.
    pub apply_lines: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrecheckStatus {
    Idle,
    Checking,
    /// A background Precheck failed (RBAC 403, network): shown quietly.
    Unavailable,
}

/// One row of the problem panel, before it is worded.
#[derive(Debug)]
struct Problem {
    doc: String,
    field: String,
    message: String,
    line: Option<usize>,
    /// Degradation / anchor annotation, e.g. "nearest existing ancestor,
    /// missing `spec.strategy`" or "value comes from anchor `base`".
    note: Option<String>,
}

/// The Edit loop's state. Synchronous: it never sends a request and never
/// reads the clock.
#[derive(Debug)]
pub struct EditLoop {
    /// The name of the Cluster Apply and Precheck go to; none until set.
    cluster: Option<String>,
    /// Bumped by [`cluster_changed`](Self::cluster_changed), so an answer
    /// from the previous Cluster is dropped.
    cluster_version: u64,
    /// Bumped by each edit; a Precheck answer for an older buffer is
    /// dropped.
    buffer_version: u64,
    /// Bumped when an Apply starts.
    apply_count: u64,
    applying: bool,
    /// The buffer being applied; errors are mapped on it, since it is what
    /// the server saw.
    pending_snapshot: Option<String>,
    // The Apply result: replaced when the next Apply starts.
    panel: Vec<Problem>,
    conflict_managers: Vec<String>,
    force_apply_available: bool,
    /// The Apply's global error lines: one per failure (a multi-document
    /// Apply can fail several documents). Replaced when the next Apply
    /// starts; a Precheck never touches them.
    apply_notices: Vec<String>,
    status: String,
    // The Precheck result: voided by an edit.
    precheck_status: PrecheckStatus,
    precheck_note: Option<String>,
    precheck_marks: Vec<(usize, Severity)>,
    /// The notices of a manual Precheck (failure, no Cluster, a Global
    /// outcome). Replaced when the next manual Precheck or an Apply starts.
    precheck_notices: Vec<String>,
}

impl Default for EditLoop {
    fn default() -> Self {
        Self::new()
    }
}

impl EditLoop {
    /// A loop with no Cluster.
    pub fn new() -> Self {
        Self {
            cluster: None,
            cluster_version: 0,
            buffer_version: 0,
            apply_count: 0,
            applying: false,
            pending_snapshot: None,
            panel: Vec::new(),
            conflict_managers: Vec::new(),
            force_apply_available: false,
            apply_notices: Vec::new(),
            status: "no cluster".to_string(),
            precheck_status: PrecheckStatus::Idle,
            precheck_note: None,
            precheck_marks: Vec::new(),
            precheck_notices: Vec::new(),
        }
    }

    // ------------------------------------------------------------ apply

    /// Start an Apply of `buffer`. `None` when one is already running, or
    /// when there is no Cluster (the notice says so).
    pub fn begin_apply(&mut self, buffer: &str, mode: ApplyMode) -> Option<Ticket> {
        if self.applying {
            return None;
        }
        if self.cluster.is_none() {
            self.apply_notices = vec![no_cluster_notice("apply")];
            return None;
        }
        self.applying = true;
        self.pending_snapshot = Some(buffer.to_string());
        // The next Apply replaces the whole previous result: panel, line
        // marks and the global error bar alike.
        self.clear_apply_result();
        // An Apply starting voids the Precheck's rehearsal, notices
        // included; a Precheck still in flight is dropped (see `Ticket`).
        self.precheck_notices.clear();
        self.apply_count += 1;
        self.status = match mode {
            ApplyMode::Normal => "applying…".to_string(),
            ApplyMode::Force => "force applying…".to_string(),
        };
        Some(self.ticket())
    }

    /// Take the answer to the Apply `ticket` was issued for. `current_buffer`
    /// is the buffer now, which may have moved on while the Apply was in
    /// flight: causes are re-mapped against it, best-effort.
    pub fn finish_apply(
        &mut self,
        ticket: Ticket,
        result: Result<ApplyReport, ClusterError>,
        current_buffer: &str,
    ) {
        if ticket.cluster_version != self.cluster_version {
            return;
        }
        self.applying = false;
        // The server gave an authoritative answer: the Precheck's rehearsal
        // is void whatever the outcome.
        self.clear_precheck();

        let mut report = match result {
            Ok(report) => report,
            Err(e) => {
                self.pending_snapshot = None;
                self.apply_notices = vec![failure_notice("apply", &e)];
                self.status = "apply failed".to_string();
                return;
            }
        };
        if let Some(snapshot) = self.pending_snapshot.take()
            && snapshot != current_buffer
        {
            report.remap(current_buffer);
        }
        if report.ok() {
            self.status = "applied".to_string();
            return;
        }

        let mut errors = 0;
        for outcome in &report.outcomes {
            let Err(error) = &outcome.result else {
                continue;
            };
            match &error.error {
                ApplyError::Global { message, .. } => {
                    self.apply_notices
                        .push(format!("{}: {message}", outcome.doc));
                }
                ApplyError::Validation(_) => {
                    errors += error.causes.len();
                    self.push_problems(outcome);
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
                        // The message failed to parse: degrade to the
                        // global bar.
                        self.apply_notices.push(format!(
                            "{}: apply conflicted (unparsable details); force apply available",
                            outcome.doc
                        ));
                    }
                    self.push_problems(outcome);
                }
            }
        }
        self.status = format!("apply failed: {errors} problem(s)");
    }

    fn push_problems(&mut self, outcome: &DocOutcome) {
        let Err(error) = &outcome.result else {
            return;
        };
        for located in &error.causes {
            self.panel
                .push(problem(outcome, &located.cause, &located.located));
        }
    }

    fn clear_apply_result(&mut self) {
        self.panel.clear();
        self.conflict_managers.clear();
        self.force_apply_available = false;
        self.apply_notices.clear();
    }

    // ------------------------------------------------------------ precheck

    /// Note an edit: it voids the Precheck result. The returned version
    /// goes to [`precheck_due`](Self::precheck_due) when the debounce timer
    /// fires.
    pub fn buffer_changed(&mut self) -> BufferVersion {
        self.buffer_version += 1;
        self.clear_precheck();
        BufferVersion(self.buffer_version)
    }

    /// Whether the debounced Precheck still runs: the buffer is as `version`
    /// left it and no Apply is in flight.
    pub fn precheck_due(&self, version: BufferVersion) -> bool {
        self.buffer_version == version.0 && !self.applying
    }

    /// Start a Precheck of `buffer`. `None` for a blank buffer, and when
    /// there is no Cluster: a `manual` one says so in a notice, the
    /// debounced one stays silent since the status already says it.
    pub fn begin_precheck(&mut self, buffer: &str, manual: bool) -> Option<Ticket> {
        if manual {
            self.precheck_notices.clear();
        }
        if buffer.trim().is_empty() {
            return None;
        }
        if self.cluster.is_none() {
            if manual {
                self.precheck_notices = vec![no_cluster_notice("precheck")];
            }
            return None;
        }
        self.precheck_status = PrecheckStatus::Checking;
        Some(self.ticket())
    }

    /// Take the answer to the Precheck `ticket` was issued for. A `manual`
    /// Precheck that fails says so in a notice; a debounced one only marks
    /// the Precheck unavailable.
    pub fn finish_precheck(
        &mut self,
        ticket: Ticket,
        manual: bool,
        result: Result<ApplyReport, ClusterError>,
    ) {
        if ticket.cluster_version != self.cluster_version {
            return;
        }
        self.precheck_status = PrecheckStatus::Idle;
        // The buffer moved on; the debounce re-checks.
        if ticket.buffer_version != self.buffer_version {
            return;
        }
        // An Apply started since: its answer voids this rehearsal.
        if ticket.apply_count != self.apply_count {
            return;
        }
        let report = match result {
            Ok(report) => report,
            Err(e) => {
                if manual {
                    self.precheck_notices = vec![failure_notice("precheck", &e)];
                } else {
                    self.precheck_status = PrecheckStatus::Unavailable;
                }
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
            if let ApplyError::Global { message, .. } = &error.error {
                unavailable.push(format!("{}: {message}", outcome.doc));
                continue;
            }
            for located in &error.causes {
                let Some(line) = located_line(&located.located) else {
                    continue;
                };
                // Strict-decoding causes (unknown/duplicate field) are
                // warnings: a real Apply ignores those fields.
                let severity = if located.cause.is_strict_decoding() {
                    warnings += 1;
                    Severity::Warning
                } else {
                    Severity::Error
                };
                marks.push((line, severity));
            }
        }
        // The outline marks are colour-only, so the warning annotation goes
        // in the status bar.
        self.precheck_note = (warnings > 0)
            .then(|| format!("{warnings} unknown/duplicate field(s) — apply will ignore them"));
        self.precheck_marks = marks;
        if !unavailable.is_empty() && manual {
            self.precheck_notices = unavailable;
        } else if !unavailable.is_empty() {
            self.precheck_status = PrecheckStatus::Unavailable;
        }
    }

    fn clear_precheck(&mut self) {
        self.precheck_status = PrecheckStatus::Idle;
        self.precheck_note = None;
        self.precheck_marks.clear();
    }

    // ------------------------------------------------------------ cluster

    /// Apply and Precheck go to Cluster `name` from now on. The last
    /// Apply's result and the last Precheck's describe the previous
    /// Cluster, so they are cleared (a Force Apply would overwrite this
    /// Cluster's fields); the buffer is kept, and answers still in flight
    /// are dropped.
    pub fn cluster_changed(&mut self, name: &str) {
        // The Session connects lazily and Apply waits on it, so the status
        // names where Apply goes rather than a connection state.
        self.status = format!("cluster {name}");
        self.cluster = Some(name.to_string());
        self.cluster_version += 1;
        self.applying = false;
        self.pending_snapshot = None;
        self.clear_apply_result();
        self.precheck_notices.clear();
        self.clear_precheck();
    }

    // ------------------------------------------------------------ display

    /// A click on the global notice: clears both groups.
    pub fn dismiss_notices(&mut self) {
        self.apply_notices.clear();
        self.precheck_notices.clear();
    }

    /// What the editor draws.
    pub fn projection(&self) -> Projection {
        let panel = (!self.panel.is_empty() || !self.conflict_managers.is_empty())
            .then(|| self.panel_projection());
        let precheck = match self.precheck_status {
            PrecheckStatus::Idle => self.precheck_note.clone().unwrap_or_default(),
            PrecheckStatus::Checking => "checking…".to_string(),
            PrecheckStatus::Unavailable => "● precheck unavailable".to_string(),
        };
        let lines: Vec<&str> = self
            .apply_notices
            .iter()
            .chain(&self.precheck_notices)
            .map(String::as_str)
            .collect();
        Projection {
            notice: (!lines.is_empty())
                .then(|| Notice::new("global-notice", lines.join("\n"), Intent::Danger)),
            panel,
            status: Notice::new("yaml-status", self.status.clone(), Intent::Muted),
            precheck: Notice::new("yaml-precheck", precheck, Intent::Muted),
            precheck_marks: self.precheck_marks.clone(),
            apply_lines: self.panel.iter().filter_map(|item| item.line).collect(),
        }
    }

    fn panel_projection(&self) -> ProblemPanel {
        let mut header = format!("{} problem(s) from last apply", self.panel.len());
        if !self.conflict_managers.is_empty() {
            let _ = write!(
                header,
                " — conflicts with {}",
                self.conflict_managers.join(", ")
            );
        }
        let rows = self
            .panel
            .iter()
            .map(|item| {
                let note = item
                    .note
                    .as_ref()
                    .map(|n| format!(" — {n}"))
                    .unwrap_or_default();
                ProblemRow {
                    text: format!(
                        "error [{}] {}: {}{}",
                        item.doc, item.field, item.message, note
                    ),
                    line: item.line,
                }
            })
            .collect();
        ProblemPanel {
            header,
            rows,
            force_apply: self.force_apply_available,
        }
    }

    fn ticket(&self) -> Ticket {
        Ticket {
            cluster_version: self.cluster_version,
            buffer_version: self.buffer_version,
            apply_count: self.apply_count,
        }
    }
}

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

fn located_line(located: &Located) -> Option<usize> {
    match located.location {
        Location::Exact { line }
        | Location::Ancestor { line, .. }
        | Location::DocHeader { line } => Some(line),
        Location::Global => None,
    }
}

fn problem(outcome: &DocOutcome, cause: &FieldCause, located: &Located) -> Problem {
    let note = match &located.location {
        Location::Ancestor { missing, .. } => Some(format!(
            "nearest existing ancestor; `{missing}` not found in manifest"
        )),
        Location::DocHeader { .. } => {
            Some("cannot locate precisely; at document start".to_string())
        }
        _ => located
            .anchor_note
            .as_ref()
            .map(|a| format!("value comes from anchor `{a}`")),
    };
    Problem {
        doc: outcome.doc.clone(),
        field: cause.raw_field.clone(),
        message: cause.message.clone(),
        line: located_line(located),
        note,
    }
}

#[cfg(test)]
mod tests {
    use astrolabe_kube::apply::{DocError, LocatedCause, ManagerConflict, Segment};

    use super::*;

    /// The lines of the global notice; none when there is none.
    fn lines(shown: &Projection) -> Vec<String> {
        shown
            .notice
            .iter()
            .flat_map(|notice| notice.text.split('\n').map(str::to_string))
            .collect()
    }

    const BUFFER: &str = "kind: ConfigMap\ndata:\n  a: 1\n";

    fn ready() -> EditLoop {
        let mut edit = EditLoop::new();
        edit.cluster_changed("kind-a");
        edit
    }

    fn cause(field: &str, message: &str) -> LocatedCause {
        LocatedCause {
            cause: FieldCause {
                raw_field: field.to_string(),
                path: Vec::new(),
                message: message.to_string(),
                reason: String::new(),
            },
            located: Located {
                location: Location::Exact { line: 3 },
                anchor_note: None,
            },
        }
    }

    fn failed(doc: &str, error: ApplyError, causes: Vec<LocatedCause>) -> DocOutcome {
        DocOutcome {
            doc_index: 0,
            doc: doc.to_string(),
            result: Err(DocError { error, causes }),
        }
    }

    fn conflict(manager: &str) -> ManagerConflict {
        ManagerConflict {
            manager: manager.to_string(),
            group_version: "v1".to_string(),
            causes: Vec::new(),
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn report(outcomes: Vec<DocOutcome>) -> Result<ApplyReport, ClusterError> {
        Ok(ApplyReport::new(outcomes))
    }

    fn apply_with(edit: &mut EditLoop, result: Result<ApplyReport, ClusterError>) {
        let ticket = edit.begin_apply(BUFFER, ApplyMode::Normal).unwrap();
        edit.finish_apply(ticket, result, BUFFER);
    }

    #[test]
    fn a_conflict_offers_force_apply_and_lists_each_manager_once() {
        let mut edit = ready();
        let doc = |name: &str, managers: &[&str]| {
            failed(
                name,
                ApplyError::Conflict(managers.iter().map(|m| conflict(m)).collect()),
                vec![cause(".data.a", "conflict")],
            )
        };
        apply_with(
            &mut edit,
            report(vec![
                doc("v1 ConfigMap/a", &["kubectl", "helm"]),
                doc("v1 ConfigMap/b", &["kubectl"]),
            ]),
        );

        let shown = edit.projection();
        let panel = shown.panel.clone().unwrap();
        assert!(panel.force_apply);
        assert_eq!(
            panel.header,
            "2 problem(s) from last apply — conflicts with kubectl, helm"
        );
        assert_eq!(shown.status.text, "apply failed: 2 problem(s)");
        assert_eq!(shown.apply_lines, [3, 3]);
    }

    #[test]
    fn an_unparsable_conflict_falls_back_to_a_global_notice() {
        let mut edit = ready();
        apply_with(
            &mut edit,
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Conflict(Vec::new()),
                Vec::new(),
            )]),
        );

        let shown = edit.projection();
        assert_eq!(
            lines(&shown),
            ["v1 ConfigMap/a: apply conflicted (unparsable details); force apply available"]
        );
        assert_eq!(shown.status.text, "apply failed: 0 problem(s)");
        // Force Apply stays available but is drawn with the panel only.
        assert!(shown.panel.is_none());
    }

    #[test]
    fn a_validation_row_says_where_it_is_and_a_global_one_goes_to_the_notices() {
        let mut edit = ready();
        let mut missing = cause("spec.strategy", "Required value");
        missing.located.location = Location::Ancestor {
            line: 2,
            missing: "strategy".to_string(),
        };
        let mut unlocated = cause("spec.x", "bad");
        unlocated.located.location = Location::Global;
        let mut header = cause("spec.y", "worse");
        header.located.location = Location::DocHeader { line: 1 };
        apply_with(
            &mut edit,
            report(vec![
                failed(
                    "apps/v1 Deployment/d",
                    ApplyError::Validation(Vec::new()),
                    vec![missing, unlocated, header],
                ),
                failed(
                    "v1 Namespace/n",
                    ApplyError::Global {
                        message: "forbidden".to_string(),
                        kind: ClusterErrorKind::Forbidden,
                    },
                    Vec::new(),
                ),
            ]),
        );

        let shown = edit.projection();
        let panel = shown.panel.clone().unwrap();
        assert!(!panel.force_apply);
        assert_eq!(panel.header, "3 problem(s) from last apply");
        assert_eq!(shown.status.text, "apply failed: 3 problem(s)");
        assert_eq!(
            panel.rows,
            [
                ProblemRow {
                    text: "error [apps/v1 Deployment/d] spec.strategy: Required value — nearest \
                           existing ancestor; `strategy` not found in manifest"
                        .to_string(),
                    line: Some(2),
                },
                ProblemRow {
                    text: "error [apps/v1 Deployment/d] spec.x: bad".to_string(),
                    line: None,
                },
                ProblemRow {
                    text: "error [apps/v1 Deployment/d] spec.y: worse — cannot locate precisely; \
                           at document start"
                        .to_string(),
                    line: Some(1),
                },
            ]
        );
        assert_eq!(lines(&shown), ["v1 Namespace/n: forbidden"]);
        assert_eq!(shown.apply_lines, [2, 1]);
    }

    #[test]
    fn a_clean_apply_says_applied_and_a_failed_one_its_error() {
        let mut edit = ready();
        let ticket = edit.begin_apply(BUFFER, ApplyMode::Force).unwrap();
        assert_eq!(edit.projection().status.text, "force applying…");
        edit.finish_apply(ticket, report(Vec::new()), BUFFER);
        assert_eq!(edit.projection().status.text, "applied");

        let ticket = edit.begin_apply(BUFFER, ApplyMode::Normal).unwrap();
        assert_eq!(edit.projection().status.text, "applying…");
        edit.finish_apply(ticket, Err(ClusterError::connect("refused")), BUFFER);
        let shown = edit.projection();
        assert_eq!(shown.status.text, "apply failed");
        assert_eq!(
            lines(&shown),
            ["cannot apply: cluster unreachable (refused)"]
        );
    }

    #[test]
    fn a_second_apply_does_not_start_while_one_runs() {
        let mut edit = ready();
        edit.begin_apply(BUFFER, ApplyMode::Normal).unwrap();
        assert!(edit.begin_apply(BUFFER, ApplyMode::Normal).is_none());
        assert_eq!(lines(&edit.projection()), Vec::<String>::new());
    }

    #[test]
    fn with_no_cluster_nothing_starts_and_the_notice_says_so() {
        let mut edit = EditLoop::new();
        assert_eq!(edit.projection().status.text, "no cluster");

        assert!(edit.begin_apply(BUFFER, ApplyMode::Normal).is_none());
        assert_eq!(lines(&edit.projection()), ["cannot apply: no cluster"]);

        edit.dismiss_notices();
        assert!(edit.begin_precheck(BUFFER, false).is_none());
        assert_eq!(lines(&edit.projection()), Vec::<String>::new());

        assert!(edit.begin_precheck(BUFFER, true).is_none());
        assert_eq!(lines(&edit.projection()), ["cannot precheck: no cluster"]);
    }

    #[test]
    fn a_blank_buffer_is_not_prechecked() {
        let mut edit = ready();
        assert!(edit.begin_precheck("  \n", true).is_none());
        assert_eq!(lines(&edit.projection()), Vec::<String>::new());
    }

    #[test]
    fn strict_decoding_causes_in_a_precheck_are_warnings() {
        let mut edit = ready();
        let ticket = edit.begin_precheck(BUFFER, false).unwrap();
        assert_eq!(edit.projection().precheck.text, "checking…");
        let mut bogus = cause("spec.bogus", "unknown field \"spec.bogus\"");
        bogus.located.location = Location::Exact { line: 2 };
        edit.finish_precheck(
            ticket,
            false,
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Validation(Vec::new()),
                vec![bogus, cause("data.a", "Invalid value")],
            )]),
        );

        let shown = edit.projection();
        assert_eq!(
            shown.precheck_marks,
            [(2, Severity::Warning), (3, Severity::Error)]
        );
        assert_eq!(
            shown.precheck.text,
            "1 unknown/duplicate field(s) — apply will ignore them"
        );
        assert!(shown.panel.is_none());
    }

    #[test]
    fn a_precheck_with_only_errors_has_no_unknown_field_note() {
        let mut edit = ready();
        let ticket = edit.begin_precheck(BUFFER, false).unwrap();
        edit.finish_precheck(
            ticket,
            false,
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Validation(Vec::new()),
                vec![cause("data.a", "Invalid value")],
            )]),
        );

        let shown = edit.projection();
        assert_eq!(shown.precheck_marks, [(3, Severity::Error)]);
        assert_eq!(shown.precheck.text, "");
    }

    #[test]
    fn an_apply_answered_after_cluster_changed_is_dropped() {
        let mut edit = ready();
        let ticket = edit.begin_apply(BUFFER, ApplyMode::Normal).unwrap();
        edit.cluster_changed("kind-b");

        edit.finish_apply(
            ticket,
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Conflict(vec![conflict("kubectl")]),
                vec![cause(".data.a", "conflict")],
            )]),
            BUFFER,
        );

        let shown = edit.projection();
        assert!(shown.panel.is_none());
        assert_eq!(shown.status.text, "cluster kind-b");
        // Not stuck applying: the new Cluster takes an Apply.
        assert!(edit.begin_apply(BUFFER, ApplyMode::Normal).is_some());
    }

    #[test]
    fn a_precheck_answered_for_an_older_buffer_is_dropped() {
        let mut edit = ready();
        let ticket = edit.begin_precheck(BUFFER, true).unwrap();
        edit.buffer_changed();

        edit.finish_precheck(
            ticket,
            true,
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Validation(Vec::new()),
                vec![cause("data.a", "Invalid value")],
            )]),
        );

        let shown = edit.projection();
        assert_eq!(shown.precheck_marks, Vec::<(usize, Severity)>::new());
        assert_eq!(shown.precheck.text, "");
    }

    #[test]
    fn a_precheck_answered_after_cluster_changed_is_dropped() {
        let mut edit = ready();
        let ticket = edit.begin_precheck(BUFFER, true).unwrap();
        edit.cluster_changed("kind-b");
        edit.finish_precheck(ticket, true, Err(ClusterError::new("late")));
        assert_eq!(lines(&edit.projection()), Vec::<String>::new());
    }

    #[test]
    fn the_debounced_precheck_does_not_run_while_an_apply_is_in_flight() {
        let mut edit = ready();
        let version = edit.buffer_changed();
        assert!(edit.precheck_due(version));

        let ticket = edit.begin_apply(BUFFER, ApplyMode::Normal).unwrap();
        assert!(!edit.precheck_due(version));

        edit.finish_apply(ticket, report(Vec::new()), BUFFER);
        assert!(edit.precheck_due(version));
        edit.buffer_changed();
        assert!(!edit.precheck_due(version));
    }

    #[test]
    fn an_apply_result_is_cleared_when_the_next_apply_starts() {
        let mut edit = ready();
        apply_with(
            &mut edit,
            report(vec![
                failed(
                    "v1 ConfigMap/a",
                    ApplyError::Conflict(vec![conflict("kubectl")]),
                    vec![cause(".data.a", "conflict")],
                ),
                failed(
                    "v1 ConfigMap/b",
                    ApplyError::Global {
                        message: "denied".to_string(),
                        kind: ClusterErrorKind::Other,
                    },
                    Vec::new(),
                ),
            ]),
        );
        assert!(edit.projection().panel.is_some());

        edit.begin_apply(BUFFER, ApplyMode::Normal).unwrap();

        let shown = edit.projection();
        assert!(shown.panel.is_none());
        assert_eq!(lines(&shown), Vec::<String>::new());
        assert_eq!(shown.apply_lines, Vec::<usize>::new());
    }

    #[test]
    fn an_apply_result_is_cleared_on_cluster_changed() {
        let mut edit = ready();
        apply_with(
            &mut edit,
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Conflict(vec![conflict("kubectl")]),
                vec![cause(".data.a", "conflict")],
            )]),
        );

        edit.cluster_changed("kind-b");

        let shown = edit.projection();
        assert!(shown.panel.is_none());
        assert_eq!(shown.apply_lines, Vec::<usize>::new());
        assert_eq!(shown.status.text, "cluster kind-b");
    }

    #[test]
    fn a_completed_apply_voids_the_precheck() {
        let mut edit = ready();
        let ticket = edit.begin_precheck(BUFFER, false).unwrap();
        edit.finish_precheck(
            ticket,
            false,
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Validation(Vec::new()),
                vec![cause("data.a", "Invalid value")],
            )]),
        );
        assert_eq!(edit.projection().precheck_marks.len(), 1);

        apply_with(&mut edit, report(Vec::new()));

        let shown = edit.projection();
        assert_eq!(shown.precheck_marks, Vec::<(usize, Severity)>::new());
        assert_eq!(shown.precheck.text, "");
    }

    #[test]
    fn an_edit_voids_the_precheck_but_not_the_apply_result() {
        let mut edit = ready();
        apply_with(
            &mut edit,
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Validation(Vec::new()),
                vec![cause("data.a", "Invalid value")],
            )]),
        );
        let ticket = edit.begin_precheck(BUFFER, false).unwrap();
        edit.finish_precheck(ticket, false, Err(ClusterError::forbidden("rbac")));
        assert_eq!(edit.projection().precheck.text, "● precheck unavailable");

        edit.buffer_changed();

        let shown = edit.projection();
        assert_eq!(shown.precheck.text, "");
        assert!(shown.panel.is_some());
    }

    #[test]
    fn manual_and_automatic_precheck_differ_on_failure() {
        let mut edit = ready();
        let ticket = edit.begin_precheck(BUFFER, false).unwrap();
        edit.finish_precheck(ticket, false, Err(ClusterError::forbidden("rbac")));
        let shown = edit.projection();
        assert_eq!(shown.precheck.text, "● precheck unavailable");
        assert_eq!(lines(&shown), Vec::<String>::new());

        let ticket = edit.begin_precheck(BUFFER, true).unwrap();
        edit.finish_precheck(ticket, true, Err(ClusterError::forbidden("rbac")));
        let shown = edit.projection();
        assert_eq!(shown.precheck.text, "");
        assert_eq!(lines(&shown), ["rbac"]);
    }

    #[test]
    fn manual_and_automatic_precheck_differ_on_a_global_outcome() {
        let global = || {
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Global {
                    message: "denied".to_string(),
                    kind: ClusterErrorKind::Forbidden,
                },
                Vec::new(),
            )])
        };
        let mut edit = ready();
        let ticket = edit.begin_precheck(BUFFER, false).unwrap();
        edit.finish_precheck(ticket, false, global());
        let shown = edit.projection();
        assert_eq!(shown.precheck.text, "● precheck unavailable");
        assert_eq!(lines(&shown), Vec::<String>::new());

        let ticket = edit.begin_precheck(BUFFER, true).unwrap();
        edit.finish_precheck(ticket, true, global());
        assert_eq!(lines(&edit.projection()), ["v1 ConfigMap/a: denied"]);
    }

    fn global_outcome() -> Result<ApplyReport, ClusterError> {
        report(vec![failed(
            "v1 ConfigMap/a",
            ApplyError::Global {
                message: "denied".to_string(),
                kind: ClusterErrorKind::Forbidden,
            },
            Vec::new(),
        )])
    }

    fn manual_precheck(edit: &mut EditLoop, result: Result<ApplyReport, ClusterError>) {
        let ticket = edit.begin_precheck(BUFFER, true).unwrap();
        edit.finish_precheck(ticket, true, result);
    }

    #[test]
    fn a_failing_manual_precheck_leaves_the_apply_notices() {
        let mut edit = ready();
        apply_with(&mut edit, Err(ClusterError::new("apply broke")));

        manual_precheck(&mut edit, Err(ClusterError::new("precheck broke")));

        assert_eq!(lines(&edit.projection()), ["apply broke", "precheck broke"]);
    }

    #[test]
    fn a_global_outcome_of_a_manual_precheck_leaves_the_apply_notices() {
        let mut edit = ready();
        apply_with(&mut edit, Err(ClusterError::new("apply broke")));

        manual_precheck(&mut edit, global_outcome());

        assert_eq!(
            lines(&edit.projection()),
            ["apply broke", "v1 ConfigMap/a: denied"]
        );
    }

    #[test]
    fn a_manual_precheck_with_no_cluster_leaves_the_apply_notices() {
        let mut edit = ready();
        apply_with(&mut edit, Err(ClusterError::new("apply broke")));
        edit.cluster = None;

        assert!(edit.begin_precheck(BUFFER, true).is_none());

        assert_eq!(
            lines(&edit.projection()),
            ["apply broke", &no_cluster_notice("precheck")[..]]
        );
    }

    #[test]
    fn a_successful_manual_precheck_clears_the_earlier_precheck_notices_only() {
        let mut edit = ready();
        apply_with(&mut edit, Err(ClusterError::new("apply broke")));
        manual_precheck(&mut edit, Err(ClusterError::new("precheck broke")));

        manual_precheck(&mut edit, report(Vec::new()));

        assert_eq!(lines(&edit.projection()), ["apply broke"]);
    }

    #[test]
    fn a_manual_precheck_replaces_the_earlier_precheck_notices_when_it_starts() {
        let mut edit = ready();
        manual_precheck(&mut edit, Err(ClusterError::new("first")));

        edit.begin_precheck(BUFFER, true).unwrap();

        assert_eq!(lines(&edit.projection()), Vec::<String>::new());
    }

    #[test]
    fn a_manual_precheck_in_flight_when_an_apply_starts_leaves_no_notice() {
        let mut edit = ready();
        let ticket = edit.begin_precheck(BUFFER, true).unwrap();
        edit.begin_apply(BUFFER, ApplyMode::Normal).unwrap();

        edit.finish_precheck(ticket, true, Err(ClusterError::new("precheck broke")));

        assert_eq!(lines(&edit.projection()), Vec::<String>::new());
    }

    #[test]
    fn a_manual_precheck_of_a_blank_buffer_clears_the_earlier_precheck_notices() {
        let mut edit = ready();
        manual_precheck(&mut edit, Err(ClusterError::new("precheck broke")));

        assert!(edit.begin_precheck("  ", true).is_none());

        assert_eq!(lines(&edit.projection()), Vec::<String>::new());
    }

    #[test]
    fn the_next_apply_clears_both_groups() {
        let mut edit = ready();
        apply_with(&mut edit, Err(ClusterError::new("apply broke")));
        manual_precheck(&mut edit, Err(ClusterError::new("precheck broke")));
        assert_eq!(lines(&edit.projection()).len(), 2);

        edit.begin_apply(BUFFER, ApplyMode::Normal).unwrap();

        assert_eq!(lines(&edit.projection()), Vec::<String>::new());
    }

    #[test]
    fn cluster_changed_clears_both_groups() {
        let mut edit = ready();
        apply_with(&mut edit, Err(ClusterError::new("apply broke")));
        manual_precheck(&mut edit, Err(ClusterError::new("precheck broke")));

        edit.cluster_changed("other");

        assert_eq!(lines(&edit.projection()), Vec::<String>::new());
    }

    #[test]
    fn the_automatic_precheck_writes_no_notices_and_keeps_the_apply_ones() {
        let mut edit = ready();
        apply_with(&mut edit, Err(ClusterError::new("apply broke")));

        let ticket = edit.begin_precheck(BUFFER, false).unwrap();
        edit.finish_precheck(ticket, false, Err(ClusterError::new("precheck broke")));
        let ticket = edit.begin_precheck(BUFFER, false).unwrap();
        edit.finish_precheck(ticket, false, global_outcome());

        assert_eq!(lines(&edit.projection()), ["apply broke"]);
    }

    #[test]
    fn dismissing_the_notices_clears_both_groups_and_nothing_else() {
        let mut edit = ready();
        apply_with(&mut edit, Err(ClusterError::new("apply broke")));
        manual_precheck(&mut edit, Err(ClusterError::new("precheck broke")));
        edit.dismiss_notices();
        let shown = edit.projection();
        assert_eq!(lines(&shown), Vec::<String>::new());
        assert_eq!(shown.status.text, "apply failed");
    }

    #[test]
    fn an_apply_answered_for_a_buffer_that_moved_on_is_mapped_onto_the_current_one() {
        let on_a = || {
            let mut bad = cause("a", "bad");
            bad.cause.path = vec![Segment::Key("a".to_string())];
            report(vec![failed(
                "v1 ConfigMap/a",
                ApplyError::Validation(Vec::new()),
                vec![bad],
            )])
        };
        let mut edit = ready();
        // The buffer is the snapshot: the report's line stands.
        let ticket = edit.begin_apply("a: 1\n", ApplyMode::Normal).unwrap();
        edit.finish_apply(ticket, on_a(), "a: 1\n");
        assert_eq!(edit.projection().apply_lines, [3]);

        // A line was added above while the Apply ran: `a` is on line 2 now.
        let ticket = edit.begin_apply("a: 1\n", ApplyMode::Normal).unwrap();
        edit.finish_apply(ticket, on_a(), "x: 0\na: 1\n");
        assert_eq!(edit.projection().apply_lines, [2]);
    }

    #[test]
    fn the_projection_names_each_text_by_its_element_and_intent() {
        let mut edit = ready();
        edit.begin_apply(BUFFER, ApplyMode::Normal).unwrap();
        let shown = edit.projection();
        assert_eq!(shown.status.id, "yaml-status");
        assert_eq!(shown.status.intent, Intent::Muted);
        assert_eq!(shown.precheck.id, "yaml-precheck");
        assert_eq!(shown.precheck.intent, Intent::Muted);

        let mut edit = EditLoop::new();
        edit.begin_apply(BUFFER, ApplyMode::Normal);
        let notice = edit.projection().notice.unwrap();
        assert_eq!(notice.id, "global-notice");
        assert_eq!(notice.intent, Intent::Danger);
    }
}
