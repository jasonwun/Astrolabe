//! Opt-in main-thread cost report (#106): with `astrolabe::perf` at
//! `trace`, a once-a-second line counts session notifies and the panel
//! work they trigger; at any other filter nothing is reported. Synthetic
//! objects (plausible shapes, never captured from a real apiserver).

mod support;

use std::time::Duration;

use astrolabe::builtin::BuiltinKind;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_kube::kube::runtime::watcher::Event;
use futures::channel::mpsc;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{Modifiers, MouseButton, TestAppContext};
use support::{LogCapture, Sender, Ui, key, object, open, open_detail, snapshot};

const REPORT: &str = "main-thread cost";

fn secret(name: &str, generation: u32) -> DynamicObject {
    object(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": "default",
            "labels": {"generation": format!("{generation}")},
        },
    }))
}

/// Let one report period pass and return the report lines logged in it.
fn next_reports(ui: &mut Ui<'_>, log: &LogCapture, seen: &mut usize) -> Vec<String> {
    ui.cx.run_until_parked();
    ui.cx.executor().advance_clock(Duration::from_secs(1));
    ui.cx.run_until_parked();
    let lines: Vec<String> = log
        .text()
        .lines()
        .filter(|line| line.contains(REPORT))
        .map(str::to_string)
        .collect();
    let new = lines[*seen..].to_vec();
    *seen = lines.len();
    new
}

/// `name=<n>`'s value in a report line.
fn field(line: &str, name: &str) -> f64 {
    let prefix = format!("{name}=");
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("no {name} in {line}"))
        .parse()
        .unwrap()
}

fn churn(ui: &mut Ui<'_>, secrets: &mut Sender<DynamicObject>, changes: u32) {
    for generation in 1..=changes {
        secrets
            .try_send(Ok(Event::Apply(secret("db", generation))))
            .unwrap();
        ui.cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn a_report_counts_the_list_refreshes_an_unrelated_watch_triggers(cx: &mut TestAppContext) {
    let log = LogCapture::with_filter("warn,astrolabe::perf=trace");
    log.scoped(|| {
        let (mut secrets, secret_events) = mpsc::channel(64);
        let (mut config_maps, config_map_events) = mpsc::channel(64);
        let mut ui = open(
            cx,
            vec![
                (key(BuiltinKind::Secrets, "default"), secret_events),
                (key(BuiltinKind::ConfigMaps, "default"), config_map_events),
            ],
        );
        ui.cx.update(|_, cx| astrolabe::perf::start(cx));
        open_detail(
            &mut ui,
            BuiltinKind::Secrets,
            &mut secrets,
            vec![secret("db", 0)],
            "db",
        );
        ui.frame(|window, cx| window.click(BuiltinKind::ConfigMaps.nav_id(), cx));
        snapshot(&mut config_maps, vec![]);
        let mut seen = 0;
        next_reports(&mut ui, &log, &mut seen);

        // The list shows ConfigMaps and the Secret detail is hidden: every
        // Secret change still refreshes the list.
        churn(&mut ui, &mut secrets, 5);
        let reports = next_reports(&mut ui, &log, &mut seen);
        assert_eq!(reports.len(), 1, "{reports:?}");
        let report = &reports[0];
        assert_eq!(field(report, "watch_events"), 5., "{report}");
        assert_eq!(field(report, "notifies"), 5., "{report}");
        assert!(field(report, "list_refresh") >= 5., "{report}");
        assert_eq!(field(report, "detail_resolve"), 0., "{report}");

        // With the detail shown, each change resolves it again.
        let detail_tab = ui.cx.debug_bounds("dock-tab-1").expect("the detail tab");
        ui.cx
            .simulate_mouse_down(detail_tab.center(), MouseButton::Left, Modifiers::none());
        ui.cx
            .simulate_mouse_up(detail_tab.center(), MouseButton::Left, Modifiers::none());
        next_reports(&mut ui, &log, &mut seen);
        churn(&mut ui, &mut secrets, 3);
        ui.frame(|_, _| {});
        let report = &next_reports(&mut ui, &log, &mut seen)[0];
        assert!(field(report, "detail_resolve") >= 1., "{report}");
        assert!(field(report, "detail_render") >= 1., "{report}");
    });
}

#[gpui_kit::test]
fn nothing_is_reported_below_trace(cx: &mut TestAppContext) {
    // `astrolabe=debug` is the watch-recovery debugging filter (#31).
    let log = LogCapture::with_filter("astrolabe=debug");
    log.scoped(|| {
        let (mut secrets, secret_events) = mpsc::channel(64);
        let mut ui = open(
            cx,
            vec![(key(BuiltinKind::Secrets, "default"), secret_events)],
        );
        ui.cx.update(|_, cx| astrolabe::perf::start(cx));
        open_detail(
            &mut ui,
            BuiltinKind::Secrets,
            &mut secrets,
            vec![secret("db", 0)],
            "db",
        );
        churn(&mut ui, &mut secrets, 3);
        let mut seen = 0;
        assert!(next_reports(&mut ui, &log, &mut seen).is_empty());
    });
}
