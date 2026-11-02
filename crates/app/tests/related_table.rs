//! A detail tab's Related table fits its columns to the section's width by
//! the related kind's weights, as the list does, and scrolls sideways below
//! the column minimums (#98). Synthetic objects (plausible shapes, never
//! captured from a real apiserver).

mod support;

use astrolabe_kube::k8s_openapi::jiff::Timestamp;
use astrolabe_kube::kube::core::DynamicObject;
use astrolabe_model::builtin::BuiltinKind;
use astrolabe_model::projection::{MIN_COLUMN_WIDTH, column_widths};
use futures::channel::mpsc;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{Bounds, Pixels, TestAppContext, Window, point, px, size};
use support::{Ui, key, object, open, open_detail, snapshot};

/// The Pods list's columns: Name, Ready, Status, Restarts, IP, Node, Age.
const POD_COLUMNS: usize = 7;

/// Synthetic Deployment (plausible shape, not captured from a server).
fn web_deployment() -> DynamicObject {
    object(serde_json::json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-deploy"},
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": "web"}},
            "template": {"metadata": {"labels": {"app": "web"}}}
        }
    }))
}

/// Synthetic Pod (plausible shape, not captured from a server).
fn web_pod() -> DynamicObject {
    object(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "web-1", "namespace": "default", "uid": "uid-web-1",
                     "labels": {"app": "web"}},
        "spec": {"nodeName": "node-a", "containers": [{"name": "app", "image": "nginx"}]},
        "status": {"phase": "Running", "podIP": "10.0.0.7"}
    }))
}

/// A Deployment's detail with one related Pod, in a window `width` wide.
fn deployment_with_a_pod(cx: &mut TestAppContext, width: f32) -> Ui<'_> {
    let (mut deployments, deployment_events) = mpsc::channel(32);
    let (mut pods, pod_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![
            (key(BuiltinKind::Deployments, "default"), deployment_events),
            (key(BuiltinKind::Pods, "default"), pod_events),
        ],
    );
    ui.cx.simulate_resize(size(px(width), px(1600.)));
    open_detail(
        &mut ui,
        BuiltinKind::Deployments,
        &mut deployments,
        vec![web_deployment()],
        "web",
    );
    snapshot(&mut pods, vec![web_pod()]);
    ui.settle();
    // The new table measures its width while it paints and refits its
    // columns on the next frame.
    ui.frame(|_, _| ());
    ui
}

fn related_bounds(
    window: &mut Window,
    id: impl Into<gpui_kit::ElementId> + Clone,
) -> Bounds<Pixels> {
    let related = window.within("detail-related");
    let name = format!("{:?}", id.clone().into());
    related
        .try_find(id.into())
        .unwrap_or_else(|| panic!("{name} not rendered under detail-related"))
        .bounds()
}

#[gpui_kit::test]
fn related_columns_share_the_width_by_the_related_kinds_weights(cx: &mut TestAppContext) {
    let mut ui = deployment_with_a_pod(cx, 1200.);

    // The weights the Pods Registration gives its own list.
    let weights = (BuiltinKind::Pods.registration().list)(&[], Timestamp::UNIX_EPOCH).weights();
    assert_eq!(weights.len(), POD_COLUMNS);

    ui.frame(|window, _| {
        let panel = window.find("detail-tab").bounds();
        let section = window.find("detail-related").bounds();
        let table = related_bounds(window, "related-table");
        // The table's 1px border on each side.
        let inner = f32::from(table.size.width) - 2.;
        let expected = column_widths(inner, &weights);
        for (index, width) in expected.iter().enumerate() {
            let header = related_bounds(window, ("col-header", index));
            assert!(
                (f32::from(header.size.width) - width).abs() <= 1.,
                "column {index}: {header:?}, expected width {width}"
            );
        }
        // Age ends at the table's inner edge, and the table sits in the
        // detail tab.
        let age = related_bounds(window, ("col-header", POD_COLUMNS - 1));
        let inner_right = table.right() - px(1.);
        assert!(
            (age.right() - inner_right).abs() <= px(1.),
            "{age:?} {table:?}"
        );
        assert!(section.right() <= panel.right(), "{section:?} {panel:?}");
        assert!(section.left() >= panel.left(), "{section:?} {panel:?}");
    });
}

#[gpui_kit::test]
fn the_related_table_is_only_as_tall_as_its_rows(cx: &mut TestAppContext) {
    let mut ui = deployment_with_a_pod(cx, 1200.);

    ui.frame(|window, _| {
        let table = related_bounds(window, "related-table");
        let header = related_bounds(window, ("col-header", 0usize));
        let cell = related_bounds(window, "table-cell-0:0");
        assert!(cell.top() >= header.bottom(), "{cell:?} {header:?}");
        // Below the only row: its bottom border and the table's.
        let band = table.bottom() - cell.bottom();
        assert!(
            band >= px(0.) && band <= px(2.),
            "{table:?} last cell {cell:?}"
        );
    });
}

#[gpui_kit::test]
fn below_the_minimum_widths_the_related_table_scrolls_to_its_last_column(cx: &mut TestAppContext) {
    let mut ui = deployment_with_a_pod(cx, 700.);

    let (table, section, panel) = ui.frame(|window, _| {
        (
            related_bounds(window, "related-table"),
            window.find("detail-related").bounds(),
            window.find("detail-tab").bounds(),
        )
    });
    assert!(
        table.size.width < px(POD_COLUMNS as f32 * MIN_COLUMN_WIDTH),
        "{table:?}"
    );
    // Neither the section nor the detail tab grows to hold the table.
    assert!(table.right() <= section.right(), "{table:?} {section:?}");
    assert!(section.right() <= panel.right(), "{section:?} {panel:?}");
    // Age starts out of view: past the table's edge, or not drawn at all.
    ui.frame(|window, _| {
        let age = window
            .within("detail-related")
            .try_find(("col-header", POD_COLUMNS - 1))
            .map(|age| age.bounds());
        assert!(
            age.is_none_or(|age| age.left() >= table.right()),
            "{age:?} {table:?}"
        );
    });

    ui.cx.simulate_event(gpui_kit::ScrollEvent {
        position: table.center(),
        delta: gpui_kit::ScrollDelta::Pixels(point(px(-1000.), px(0.))),
        ..Default::default()
    });
    ui.settle();
    ui.frame(|window, _| {
        let age = related_bounds(window, ("col-header", POD_COLUMNS - 1));
        assert!(age.size.width >= px(MIN_COLUMN_WIDTH), "{age:?}");
        assert!(age.left() >= table.left(), "{age:?} {table:?}");
        assert!(age.right() <= table.right(), "{age:?} {table:?}");
    });
}
