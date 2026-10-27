//! PROTOTYPE / THROWAWAY — DataTable performance spike. Not production code.
//!
//! Question: does gpui-kit's DataTable (uniform_list-backed) stay smooth with
//! ~10k Pod rows while a synthetic watch stream applies ~20 batches/sec of
//! incremental add/update/delete mutations, under interactive scroll/sort/
//! select? Keys 1-4 switch the mutation coalescing window.
//! `SPIKE_AUTO=1 cargo run --release` drives it on a fixed schedule, prints a
//! summary and `RESULT: PASS/FAIL`, and quits (see `auto.rs`).

mod auto;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui_kit::component::{
    ActiveTheme as _, h_flex,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex,
};
use gpui_kit::profiler::{FrameEvent, FrameTimingCollector};
use gpui_kit::*;

gpui_kit::actions!(spike, [Batch50, Batch100, Batch250, Batch500]);

const KEY_CONTEXT: &str = "spike";
const TICK: Duration = Duration::from_millis(50); // 20 Hz synthetic watch stream
const TARGET_ROWS: usize = 10_000;
const FRAME_WINDOW: Duration = Duration::from_secs(1);
const STATS_EVERY: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------- rng (splitmix64; no rand dep)

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

// ---------------------------------------------------------------- data model

const PREFIXES: &[&str] = &[
    "nginx",
    "redis",
    "api",
    "worker",
    "cache",
    "gateway",
    "scheduler",
    "auth",
    "billing",
    "search",
    "metrics",
    "queue",
];
const NAMESPACES: &[&str] = &[
    "default",
    "kube-system",
    "staging",
    "prod",
    "monitoring",
    "ingress",
];
const STATUSES: &[&str] = &[
    "Running",
    "Running",
    "Running",
    "Running",
    "Pending",
    "CrashLoopBackOff",
    "Completed",
    "Terminating",
    "ImagePullBackOff",
];
const NODES: &[&str] = &[
    "ip-10-0-1-21.ec2.internal",
    "ip-10-0-1-87.ec2.internal",
    "ip-10-0-2-14.ec2.internal",
    "ip-10-0-2-233.ec2.internal",
    "ip-10-0-3-5.ec2.internal",
];

#[derive(Clone)]
struct Pod {
    id: u64,
    name: SharedString,
    namespace: SharedString,
    status: SharedString,
    restarts: u32,
    cpu_m: u32,
    mem_mi: u32,
    age_s: u64,
    node: SharedString,
}

fn random_name(rng: &mut Rng) -> String {
    let prefix = PREFIXES[rng.below(PREFIXES.len())];
    let hex = format!("{:09x}", rng.next() & 0xF_FFFF_FFFF);
    let mut suffix = String::with_capacity(5);
    for _ in 0..5 {
        let c = (rng.next() % 36) as u8;
        suffix.push(if c < 10 { b'0' + c } else { b'a' + c - 10 } as char);
    }
    format!("{prefix}-{hex}-{suffix}")
}

fn random_pod(rng: &mut Rng, id: u64) -> Pod {
    Pod {
        id,
        name: random_name(rng).into(),
        namespace: NAMESPACES[rng.below(NAMESPACES.len())].into(),
        status: STATUSES[rng.below(STATUSES.len())].into(),
        restarts: rng.below(12) as u32,
        cpu_m: 1 + rng.below(1800) as u32,
        mem_mi: 16 + rng.below(4096) as u32,
        age_s: rng.next() % (90 * 24 * 3600),
        node: NODES[rng.below(NODES.len())].into(),
    }
}

fn fmt_age(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    } else if secs >= 3_600 {
        format!("{}h{}m", secs / 3_600, (secs % 3_600) / 60)
    } else {
        format!("{}m{}s", secs / 60, secs % 60)
    }
}

// ---------------------------------------------------------------- synthetic watch mutations

enum Mutation {
    Add(Pod),
    Delete(u64),      // by pod id — indices go stale inside a coalescing window
    Update(u64, Pod), // whole-row replacement
}

/// One 50 ms tick of the fake watch stream: 1-4 mutations, ~65% updates,
/// adds/deletes biased to keep the count oscillating around TARGET_ROWS.
fn gen_batch(rng: &mut Rng, pods: &[Pod], next_id: &mut u64) -> Vec<Mutation> {
    let n = 1 + rng.below(4);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let roll = rng.below(100);
        let want_add = if pods.len() >= TARGET_ROWS + 500 {
            false
        } else if pods.len() <= TARGET_ROWS - 500 {
            true
        } else {
            roll < 15
        };
        let want_del = !want_add && roll >= 85 && pods.len() > TARGET_ROWS - 500;

        if want_add {
            *next_id += 1;
            out.push(Mutation::Add(random_pod(rng, *next_id)));
        } else if want_del {
            let id = pods[rng.below(pods.len())].id;
            out.push(Mutation::Delete(id));
        } else {
            let mut pod = pods[rng.below(pods.len())].clone();
            if rng.chance(15) {
                pod.status = STATUSES[rng.below(STATUSES.len())].into();
            }
            if rng.chance(10) {
                pod.restarts = pod.restarts.saturating_add(1);
            }
            pod.cpu_m = (pod.cpu_m as i64 + (rng.next() % 61) as i64 - 30).clamp(1, 2000) as u32;
            pod.mem_mi =
                (pod.mem_mi as i64 + (rng.next() % 129) as i64 - 64).clamp(16, 8192) as u32;
            out.push(Mutation::Update(pod.id, pod));
        }
    }
    out
}

fn apply_batch(delegate: &mut PodsDelegate, batch: Vec<Mutation>, stats: &mut Stats) {
    for m in batch {
        match m {
            Mutation::Add(pod) => {
                delegate.pods.push(pod);
                stats.adds += 1;
            }
            Mutation::Delete(id) => {
                if let Some(ix) = delegate.pods.iter().position(|p| p.id == id) {
                    delegate.pods.swap_remove(ix);
                    stats.deletes += 1;
                }
            }
            Mutation::Update(id, pod) => {
                if let Some(existing) = delegate.pods.iter_mut().find(|p| p.id == id) {
                    *existing = pod;
                    stats.updates += 1;
                }
            }
        }
    }
}

// ---------------------------------------------------------------- delegate

struct PodsDelegate {
    columns: Vec<Column>,
    pods: Vec<Pod>,
}

impl PodsDelegate {
    fn new(rng: &mut Rng) -> Self {
        let columns = vec![
            Column::new("name", "Name").width(px(280.)).sortable(),
            Column::new("namespace", "Namespace")
                .width(px(130.))
                .sortable(),
            Column::new("status", "Status").width(px(160.)).sortable(),
            Column::new("restarts", "Restarts")
                .width(px(90.))
                .text_right()
                .sortable(),
            Column::new("cpu", "CPU")
                .width(px(90.))
                .text_right()
                .sortable(),
            Column::new("memory", "Memory")
                .width(px(100.))
                .text_right()
                .sortable(),
            Column::new("age", "Age")
                .width(px(90.))
                .text_right()
                .sortable(),
            Column::new("node", "Node").width(px(220.)).sortable(),
        ];
        let pods = (0..TARGET_ROWS)
            .map(|i| random_pod(rng, i as u64))
            .collect();
        Self { columns, pods }
    }
}

impl TableDelegate for PodsDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.pods.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(pod) = self.pods.get(row_ix) else {
            return div().child("--");
        };
        let theme = cx.theme();
        match col_ix {
            0 => div().child(pod.name.clone()),
            1 => div()
                .text_color(theme.muted_foreground)
                .child(pod.namespace.clone()),
            2 => {
                let color = match pod.status.as_ref() {
                    "Running" => theme.green,
                    "CrashLoopBackOff" | "ImagePullBackOff" => theme.red,
                    "Pending" | "Terminating" => theme.yellow,
                    _ => theme.muted_foreground,
                };
                div().text_color(color).child(pod.status.clone())
            }
            3 => div().child(pod.restarts.to_string()),
            4 => div().child(format!("{}m", pod.cpu_m)),
            5 => div().child(format!("{}Mi", pod.mem_mi)),
            6 => div().child(fmt_age(pod.age_s)),
            7 => div()
                .text_color(theme.muted_foreground)
                .child(pod.node.clone()),
            _ => div().child("--"),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        use std::cmp::Ordering;
        let key = self.columns[col_ix].key.clone();
        let cmp = move |a: &Pod, b: &Pod| -> Ordering {
            match key.as_ref() {
                "name" => a.name.cmp(&b.name),
                "namespace" => a.namespace.cmp(&b.namespace),
                "status" => a.status.cmp(&b.status),
                "restarts" => a.restarts.cmp(&b.restarts),
                "cpu" => a.cpu_m.cmp(&b.cpu_m),
                "memory" => a.mem_mi.cmp(&b.mem_mi),
                "age" => a.age_s.cmp(&b.age_s),
                "node" => a.node.cmp(&b.node),
                _ => a.id.cmp(&b.id),
            }
        };
        match sort {
            ColumnSort::Ascending => self.pods.sort_by(|a, b| cmp(a, b)),
            ColumnSort::Descending => self.pods.sort_by(|a, b| cmp(b, a)),
            ColumnSort::Default => self.pods.sort_by_key(|p| p.id),
        }
    }
}

// ---------------------------------------------------------------- frame stats (gpui profiler trace)

/// Drains gpui's frame trace for this window; keeps ~1s of draw times,
/// present times, and present-to-present gaps (the jank metric).
///
/// The timestamps come from gpui's trace (`web_time::Instant`); pruning uses
/// `elapsed()` so this type never has to name it.
struct FrameStats {
    collector: FrameTimingCollector,
    window_id: WindowId,
    draws: VecDeque<(web_time::Instant, Duration)>,
    presents: VecDeque<web_time::Instant>,
    gaps: VecDeque<(web_time::Instant, Duration)>,
    /// Whole-run totals since the last `reset_run`, for the scripted summary.
    run: RunFrames,
}

#[derive(Default)]
struct RunFrames {
    draws: u32,
    draw_sum: Duration,
    draw_worst: Duration,
    gap_worst: Duration,
    gaps_over_limit: u32,
}

impl FrameStats {
    fn new(window: &Window) -> Self {
        Self {
            collector: FrameTimingCollector::new(),
            window_id: window.window_handle().window_id(),
            draws: VecDeque::with_capacity(256),
            presents: VecDeque::with_capacity(256),
            gaps: VecDeque::with_capacity(256),
            run: RunFrames::default(),
        }
    }

    fn drain(&mut self) {
        for event in self.collector.collect_unseen() {
            match event {
                FrameEvent::Draw(t) if t.window_id == self.window_id => {
                    let d = t.draw_duration();
                    self.draws.push_back((t.draw_start, d));
                    self.run.draws += 1;
                    self.run.draw_sum += d;
                    self.run.draw_worst = self.run.draw_worst.max(d);
                }
                FrameEvent::Present(t) if t.window_id == self.window_id => {
                    let at = t.present_end;
                    if let Some(&prev) = self.presents.back() {
                        let gap = at.duration_since(prev);
                        self.gaps.push_back((at, gap));
                        self.run.gap_worst = self.run.gap_worst.max(gap);
                        if gap > auto::GAP_LIMIT {
                            self.run.gaps_over_limit += 1;
                        }
                    }
                    self.presents.push_back(at);
                }
                _ => {}
            }
        }
        while self
            .draws
            .front()
            .is_some_and(|(t, _)| t.elapsed() > FRAME_WINDOW)
        {
            self.draws.pop_front();
        }
        while self
            .presents
            .front()
            .is_some_and(|t| t.elapsed() > FRAME_WINDOW)
        {
            self.presents.pop_front();
        }
        while self
            .gaps
            .front()
            .is_some_and(|(t, _)| t.elapsed() > FRAME_WINDOW)
        {
            self.gaps.pop_front();
        }
    }

    fn reset_run(&mut self) {
        self.drain();
        self.run = RunFrames::default();
    }

    /// (fps over last 1s, mean draw, worst draw, worst present gap)
    fn stats(&mut self) -> (f32, Duration, Duration, Duration) {
        let fps = self.presents.len() as f32 / FRAME_WINDOW.as_secs_f32();
        let n = self.draws.len() as u32;
        let (sum, worst) = self
            .draws
            .iter()
            .fold((Duration::ZERO, Duration::ZERO), |(s, w), (_, d)| {
                (s + *d, w.max(*d))
            });
        let avg = if n > 0 { sum / n } else { Duration::ZERO };
        let worst_gap = self
            .gaps
            .iter()
            .map(|(_, g)| *g)
            .max()
            .unwrap_or(Duration::ZERO);
        (fps, avg, worst, worst_gap)
    }
}

// ---------------------------------------------------------------- RSS (macOS mach)

fn rss_bytes() -> u64 {
    unsafe {
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        let rc = libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        );
        if rc == 0 { info.ri_resident_size } else { 0 }
    }
}

// ---------------------------------------------------------------- view

#[derive(Clone, Copy, PartialEq)]
enum BatchMode {
    Ms50,
    Ms100,
    Ms250,
    Ms500,
}

impl BatchMode {
    fn window(self) -> Duration {
        match self {
            Self::Ms50 => Duration::from_millis(50),
            Self::Ms100 => Duration::from_millis(100),
            Self::Ms250 => Duration::from_millis(250),
            Self::Ms500 => Duration::from_millis(500),
        }
    }
    fn ms(self) -> u32 {
        self.window().as_millis() as u32
    }
}

#[derive(Default)]
struct Stats {
    generated: u64,
    adds: u64,
    updates: u64,
    deletes: u64,
    flushes: u64,
    last_flush_applied: usize,
    max_batch: usize,
}

struct SpikeView {
    table: Entity<TableState<PodsDelegate>>,
    focus_handle: FocusHandle,
    mode: BatchMode,
    pending: Vec<Mutation>,
    rng: Rng,
    next_id: u64,
    frames: FrameStats,
    stats: Stats,
    rss: u64,
    last_rss: Instant,
    last_flush: Instant,
    started: Instant,
    last_stats_print: Instant,
    _task: Task<()>,
}

impl SpikeView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut rng = Rng(0x5EED_CAFE_1234_5678);
        let delegate = PodsDelegate::new(&mut rng);
        let table = cx.new(|cx| TableState::new(delegate, window, cx));
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        if std::env::var("SPIKE_AUTO").is_ok() {
            cx.spawn_in(window, async move |view, cx| auto::run(view, cx).await)
                .detach();
        }

        let _task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                    break;
                }
            }
        });

        let now = Instant::now();
        Self {
            table,
            focus_handle,
            mode: BatchMode::Ms50,
            pending: Vec::new(),
            rng,
            next_id: TARGET_ROWS as u64,
            frames: FrameStats::new(window),
            stats: Stats::default(),
            rss: 0,
            last_rss: now,
            last_flush: now,
            started: now,
            last_stats_print: now,
            _task,
        }
    }

    /// One 20 Hz tick: generate a mutation batch, then either apply it now or
    /// hold it until the current coalescing window elapses.
    fn tick(&mut self, cx: &mut Context<Self>) {
        let batch = {
            let delegate = self.table.read(cx).delegate();
            gen_batch(&mut self.rng, &delegate.pods, &mut self.next_id)
        };
        self.stats.generated += batch.len() as u64;
        self.pending.extend(batch);

        if self.last_flush.elapsed() >= self.mode.window() && !self.pending.is_empty() {
            let batch = std::mem::take(&mut self.pending);
            self.stats.max_batch = self.stats.max_batch.max(batch.len());
            self.stats.last_flush_applied = batch.len();
            self.stats.flushes += 1;
            let stats = &mut self.stats;
            let table = self.table.clone();
            table.update(cx, |table, cx| {
                apply_batch(table.delegate_mut(), batch, stats);
                cx.notify();
            });
            self.last_flush = Instant::now();
        }

        if self.last_rss.elapsed() >= Duration::from_secs(1) {
            self.rss = rss_bytes();
            self.last_rss = Instant::now();
        }

        if self.last_stats_print.elapsed() >= STATS_EVERY {
            let (fps, avg, worst, gap) = self.frames.stats();
            println!(
                "[spike t={:>3.0}s] rows={} generated={} applied(add/upd/del)={}/{}/{} flushes={} max_batch={} mode={}ms fps={:.0} draw_avg={:.2?} draw_worst={:.2?} gap_worst={:.2?} rss={}MB",
                self.started.elapsed().as_secs_f32(),
                self.table.read(cx).delegate().pods.len(),
                self.stats.generated,
                self.stats.adds,
                self.stats.updates,
                self.stats.deletes,
                self.stats.flushes,
                self.stats.max_batch,
                self.mode.ms(),
                fps,
                avg,
                worst,
                gap,
                self.rss / 1_048_576,
            );
            self.last_stats_print = Instant::now();
        }

        cx.notify();
    }

    fn set_mode(&mut self, mode: BatchMode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.last_flush = Instant::now();
        cx.notify();
    }
}

impl Render for SpikeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.frames.drain();
        let (fps, avg_draw, worst_draw, worst_gap) = self.frames.stats();

        let overlay = h_flex()
            .gap_4()
            .px_3()
            .py_1()
            .text_sm()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(format!("fps {fps:>3.0}"))
            .child(format!(
                "draw avg {avg_draw:.2?} worst {worst_draw:.2?} (1s)"
            ))
            .child(format!("gap worst {worst_gap:.2?}"))
            .child(format!(
                "rows {}",
                self.table.read(cx).delegate().pods.len()
            ))
            .child(format!(
                "applied {} (last flush {})",
                self.stats.adds + self.stats.updates + self.stats.deletes,
                self.stats.last_flush_applied
            ))
            .child(format!("batch {}ms [keys 1-4]", self.mode.ms()))
            .child(format!("rss {} MB", self.rss / 1_048_576));

        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Batch50, _, cx| this.set_mode(BatchMode::Ms50, cx)))
            .on_action(cx.listener(|this, _: &Batch100, _, cx| this.set_mode(BatchMode::Ms100, cx)))
            .on_action(cx.listener(|this, _: &Batch250, _, cx| this.set_mode(BatchMode::Ms250, cx)))
            .on_action(cx.listener(|this, _: &Batch500, _, cx| this.set_mode(BatchMode::Ms500, cx)))
            .child(overlay)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(DataTable::new(&self.table).stripe(true)),
            )
    }
}

fn main() {
    #[expect(
        clippy::disallowed_methods,
        reason = "a spike measures gpui-kit alone, without the app's icon assets"
    )]
    gpui_kit::application().run(|cx| {
        // Turn on gpui's frame trace so the overlay's FrameTimingCollector
        // actually receives draw/present events (off by default).
        gpui_kit::profiler::set_trace_enabled(true);
        gpui_kit::init(cx);
        cx.bind_keys([
            KeyBinding::new("1", Batch50, Some(KEY_CONTEXT)),
            KeyBinding::new("2", Batch100, Some(KEY_CONTEXT)),
            KeyBinding::new("3", Batch250, Some(KEY_CONTEXT)),
            KeyBinding::new("4", Batch500, Some(KEY_CONTEXT)),
        ]);
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| SpikeView::new(window, cx))
        })
        .expect("failed to open window");
    });
}
