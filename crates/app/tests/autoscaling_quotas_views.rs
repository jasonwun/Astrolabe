//! HorizontalPodAutoscalers, ResourceQuotas and LimitRanges (#20) through
//! the workspace UI, on a scripted cluster. Synthetic objects (plausible
//! shapes, never captured from a real apiserver).

mod support;

use astrolabe::builtin::BuiltinKind;
use futures::channel::mpsc;
use gpui_kit::TestAppContext;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use support::{key, object, open, open_detail, snapshot};

/// How far down the detail each id renders, panicking on one that does
/// not.
fn positions(window: &gpui_kit::Window, ids: &[&'static str]) -> Vec<gpui_kit::Pixels> {
    ids.iter()
        .map(|id| {
            window
                .try_find(*id)
                .unwrap_or_else(|| panic!("{id} not rendered"))
                .bounds()
                .origin
                .y
        })
        .collect()
}

#[gpui_kit::test]
fn an_hpa_detail_shows_replicas_conditions_then_metrics(cx: &mut TestAppContext) {
    let (mut hpas, hpa_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(
            key(BuiltinKind::HorizontalPodAutoscalers, "default"),
            hpa_events,
        )],
    );
    let hpa = object(serde_json::json!({
        "apiVersion": "autoscaling/v2", "kind": "HorizontalPodAutoscaler",
        "metadata": {"name": "web", "namespace": "default", "uid": "uid-hpa"},
        "spec": {
            "scaleTargetRef": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "web"},
            "minReplicas": 2, "maxReplicas": 10,
            "metrics": [{"type": "Resource", "resource": {"name": "cpu",
                "target": {"type": "Utilization", "averageUtilization": 50}}}]
        },
        "status": {
            "currentReplicas": 3, "desiredReplicas": 4,
            "currentMetrics": [{"type": "Resource", "resource": {"name": "cpu",
                "current": {"averageUtilization": 30}}}],
            "conditions": [{"type": "AbleToScale", "status": "True",
                "reason": "ReadyForNewScale", "lastTransitionTime": "2026-10-22T11:10:00Z"}]
        }
    }));
    open_detail(
        &mut ui,
        BuiltinKind::HorizontalPodAutoscalers,
        &mut hpas,
        vec![hpa],
        "web",
    );

    ui.frame(|window, _| {
        for label in [
            "Reference: Deployment/web",
            "Min Pods: 2",
            "Max Pods: 10",
            "Replicas: 3",
            "Desired Replicas: 4",
            "Condition: AbleToScale=True (ReadyForNewScale)",
        ] {
            assert!(window.try_find_by_label(label).is_some(), "{label}");
        }
        let metrics = window.within("detail-section-Metrics");
        assert!(metrics.try_find_by_label("Resource cpu on Pods").is_some());
        assert!(metrics.try_find_by_label("30% / 50%").is_some());
        let order = positions(
            window,
            &[
                "field-Reference",
                "field-Desired Replicas",
                "field-Condition",
                "detail-section-Metrics",
                "detail-yaml",
            ],
        );
        assert!(order.is_sorted(), "{order:?}");
        assert!(window.try_find("detail-edit").is_some());
        assert!(window.try_find("detail-delete").is_some());
        assert!(window.try_find("detail-related").is_none());
    });
}

#[gpui_kit::test]
fn a_resource_quota_detail_shows_quotas_then_its_scope_selector(cx: &mut TestAppContext) {
    let (mut quotas, quota_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::ResourceQuotas, "default"), quota_events)],
    );
    let quota = object(serde_json::json!({
        "apiVersion": "v1", "kind": "ResourceQuota",
        "metadata": {"name": "compute", "namespace": "default", "uid": "uid-quota"},
        "spec": {
            "hard": {"memory": "1Gi"},
            "scopeSelector": {"matchExpressions": [
                {"operator": "In", "scopeName": "PriorityClass", "values": ["high"]}
            ]}
        },
        "status": {"hard": {"memory": "1Gi"}, "used": {"memory": "256Mi"}}
    }));
    open_detail(
        &mut ui,
        BuiltinKind::ResourceQuotas,
        &mut quotas,
        vec![quota],
        "compute",
    );

    ui.frame(|window, _| {
        let table = window.within("detail-table-Quotas");
        for cell in ["memory", "256Mi", "1Gi", "25%"] {
            assert!(table.try_find_by_label(cell).is_some(), "{cell}");
        }
        let scope = window.within("detail-section-Scope Selector");
        assert!(scope.try_find_by_label("PriorityClass").is_some());
        let order = positions(
            window,
            &[
                "detail-table-Quotas",
                "detail-section-Scope Selector",
                "detail-yaml",
            ],
        );
        assert!(order.is_sorted(), "{order:?}");
        assert!(window.try_find("detail-related").is_none());
    });
}

#[gpui_kit::test]
fn a_limit_range_detail_shows_a_section_per_limit_type(cx: &mut TestAppContext) {
    let (mut limits, limit_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(key(BuiltinKind::LimitRanges, "default"), limit_events)],
    );
    let limit_range = object(serde_json::json!({
        "apiVersion": "v1", "kind": "LimitRange",
        "metadata": {"name": "defaults", "namespace": "default", "uid": "uid-limits"},
        "spec": {"limits": [
            {"type": "Container", "max": {"cpu": "2"}, "default": {"cpu": "500m"}},
            {"type": "PersistentVolumeClaim", "max": {"storage": "10Gi"}}
        ]}
    }));
    open_detail(
        &mut ui,
        BuiltinKind::LimitRanges,
        &mut limits,
        vec![limit_range],
        "defaults",
    );

    ui.frame(|window, _| {
        let container = window.within("detail-section-Container Limits");
        assert!(
            container
                .try_find_by_label("cpu: max:2, default:500m")
                .is_some()
        );
        assert!(container.try_find_by_label("memory: ").is_some());
        assert!(window.try_find("detail-section-Pod Limits").is_none());
        let order = positions(
            window,
            &[
                "detail-section-Container Limits",
                "detail-section-Persistent Volume Claim Limits",
                "detail-yaml",
            ],
        );
        assert!(order.is_sorted(), "{order:?}");
        assert!(window.try_find("detail-related").is_none());
    });
}

/// The right edge of each id's bounds, panicking on one that does not
/// render.
fn right(window: &gpui_kit::Window, id: impl Into<gpui_kit::ElementId>) -> gpui_kit::Pixels {
    let id = id.into();
    window
        .try_find(id.clone())
        .unwrap_or_else(|| panic!("{id} not rendered"))
        .bounds()
        .right()
}

#[gpui_kit::test]
fn the_hpa_list_fits_its_seven_columns_to_the_panel(cx: &mut TestAppContext) {
    let (mut hpas, hpa_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(
            key(BuiltinKind::HorizontalPodAutoscalers, "default"),
            hpa_events,
        )],
    );
    ui.cx
        .simulate_resize(gpui_kit::size(gpui_kit::px(1300.), gpui_kit::px(800.)));
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(BuiltinKind::HorizontalPodAutoscalers.nav_id(), cx));
    snapshot(
        &mut hpas,
        vec![object(serde_json::json!({
            "apiVersion": "autoscaling/v2", "kind": "HorizontalPodAutoscaler",
            "metadata": {"name": "web", "namespace": "default", "uid": "uid-hpa"},
            "spec": {
                "scaleTargetRef": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "web"},
                "maxReplicas": 10
            }
        }))],
    );
    ui.settle();

    ui.frame(|window, _| {
        let table = window.try_find("list-table").unwrap().bounds();
        assert!(table.size.width < gpui_kit::px(1280.), "{table:?}");
        let conditions = window
            .try_find("column-6")
            .expect("Conditions header")
            .bounds();
        assert!(window.try_find_by_label("Conditions").is_some());
        assert!(
            conditions.left() >= table.left(),
            "{conditions:?} {table:?}"
        );
        assert!(
            conditions.right() <= table.right(),
            "{conditions:?} {table:?}"
        );
        // The last column's cell ends at the table's inner (1px border)
        // edge: no gap after it, and nothing hidden past it.
        let last = window
            .try_find(("col-header", 6usize))
            .expect("Conditions cell")
            .bounds();
        assert_eq!(
            last.right(),
            table.right() - gpui_kit::px(1.),
            "{last:?} {table:?}"
        );
    });

    // Narrower: the columns shrink with the window.
    let wide = ui.frame(|window, _| right(window, "column-0"));
    ui.cx
        .simulate_resize(gpui_kit::size(gpui_kit::px(1100.), gpui_kit::px(800.)));
    ui.settle();
    ui.frame(|window, _| {
        assert!(right(window, "column-0") < wide);
        assert_eq!(
            right(window, ("col-header", 6usize)),
            right(window, "list-table") - gpui_kit::px(1.)
        );
    });
}

#[gpui_kit::test]
fn below_the_minimum_widths_the_hpa_list_scrolls_to_its_last_column(cx: &mut TestAppContext) {
    let (mut hpas, hpa_events) = mpsc::channel(32);
    let mut ui = open(
        cx,
        vec![(
            key(BuiltinKind::HorizontalPodAutoscalers, "default"),
            hpa_events,
        )],
    );
    ui.cx
        .simulate_resize(gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(800.)));
    ui.namespaces(&["default"]);
    ui.settle();
    ui.frame(|window, cx| window.click(BuiltinKind::HorizontalPodAutoscalers.nav_id(), cx));
    snapshot(
        &mut hpas,
        vec![object(serde_json::json!({
            "apiVersion": "autoscaling/v2", "kind": "HorizontalPodAutoscaler",
            "metadata": {"name": "web", "namespace": "default", "uid": "uid-hpa"},
            "spec": {
                "scaleTargetRef": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "web"},
                "maxReplicas": 10
            }
        }))],
    );
    ui.settle();

    let table = ui.frame(|window, _| window.try_find("list-table").unwrap().bounds());
    assert!(table.size.width < gpui_kit::px(7. * 80.), "{table:?}");
    ui.cx.simulate_event(gpui_kit::ScrollEvent {
        position: table.center(),
        delta: gpui_kit::ScrollDelta::Pixels(gpui_kit::point(
            gpui_kit::px(-1000.),
            gpui_kit::px(0.),
        )),
        ..Default::default()
    });
    ui.settle();
    ui.frame(|window, _| {
        let last = window
            .try_find(("col-header", 6usize))
            .expect("Conditions cell")
            .bounds();
        assert!(last.size.width >= gpui_kit::px(80.), "{last:?}");
        assert!(last.left() >= table.left(), "{last:?} {table:?}");
        assert!(last.right() <= table.right(), "{last:?} {table:?}");
    });
}
