use astrolabe::projection::{ResourceRow, project_namespaces};
use astrolabe::store::ObjectKey;
use astrolabe_kube::k8s_openapi::api::core::v1::{Namespace, NamespaceStatus};
use astrolabe_kube::k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

fn namespace(name: &str, phase: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..ObjectMeta::default()
        },
        status: Some(NamespaceStatus {
            phase: Some(phase.to_string()),
            ..NamespaceStatus::default()
        }),
        ..Namespace::default()
    }
}

#[test]
fn namespaces_project_to_handwritten_columns() {
    let active = namespace("default", "Active");
    let terminating = namespace("old", "Terminating");

    let rows = project_namespaces(&[&active, &terminating]);

    assert_eq!(
        rows,
        vec![
            ResourceRow {
                key: ObjectKey::cluster_scoped("default"),
                cells: vec!["default".to_string(), "Active".to_string()],
            },
            ResourceRow {
                key: ObjectKey::cluster_scoped("old"),
                cells: vec!["old".to_string(), "Terminating".to_string()],
            },
        ]
    );
}

mod namespace_detail {
    use astrolabe::projection::{namespace_detail, object_yaml};
    use astrolabe_kube::k8s_openapi::api::core::v1::Namespace;
    use astrolabe_kube::k8s_openapi::jiff::Timestamp;

    fn now() -> Timestamp {
        "2026-10-22T12:00:00Z".parse().unwrap()
    }

    /// Synthetic Namespace: plausible shape, not captured from a server.
    fn terminating() -> Namespace {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "v1", "kind": "Namespace",
            "metadata": {
                "name": "old", "uid": "uid-ns",
                "creationTimestamp": "2026-10-22T11:55:00Z",
                "labels": {"team": "a"},
                "annotations": {"note": "bye"},
                "managedFields": [{"manager": "kubectl", "operation": "Update"}]
            },
            "spec": {"finalizers": ["kubernetes"]},
            "status": {
                "phase": "Terminating",
                "conditions": [{
                    "type": "NamespaceDeletionContentFailure", "status": "False",
                    "reason": "ContentDeleted", "message": "All content successfully deleted"
                }]
            }
        }))
        .unwrap()
    }

    #[test]
    fn shows_phase_finalizers_and_conditions_on_the_generic_template() {
        let namespace = terminating();

        let detail = namespace_detail(&namespace, now());

        assert_eq!(detail.title, "Namespace old");
        assert_eq!(
            detail.metadata,
            [
                ("Name", "old"),
                ("UID", "uid-ns"),
                ("Created", "2026-10-22T11:55:00Z (5m ago)"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert_eq!(detail.labels, ["team=a"]);
        assert_eq!(detail.annotations, ["note=bye"]);
        assert_eq!(
            detail.status,
            [("Phase", "Terminating"), ("Finalizers", "kubernetes")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert_eq!(detail.conditions.len(), 1);
        assert_eq!(detail.conditions[0].kind, "NamespaceDeletionContentFailure");
        assert_eq!(detail.conditions[0].status, "False");
        assert_eq!(detail.conditions[0].reason, "ContentDeleted");
        assert_eq!(detail.yaml, object_yaml(&namespace));
        assert!(detail.yaml.contains("phase: Terminating"));
        assert!(!detail.yaml.contains("managedFields"));
    }

    #[test]
    fn a_bare_namespace_has_empty_status_rows_and_no_conditions() {
        let bare: Namespace =
            serde_json::from_value(serde_json::json!({"metadata": {"name": "new"}})).unwrap();

        let detail = namespace_detail(&bare, now());

        assert_eq!(
            detail.status,
            [("Phase", ""), ("Finalizers", "")].map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert!(detail.conditions.is_empty());
    }
}

mod custom {
    use std::collections::BTreeSet;

    use astrolabe::projection::{
        ResourceRow, age_cadence, custom_resource_detail, definition_detail, format_age,
        next_age_change, project_custom_resources, project_definitions, track_ages,
    };
    use astrolabe::store::ObjectKey;
    use astrolabe_kube::crd::{CustomKind, catalog};
    use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
    use astrolabe_kube::k8s_openapi::jiff::{SignedDuration, Timestamp};
    use astrolabe_kube::kube::core::DynamicObject;

    fn now() -> Timestamp {
        "2026-10-22T12:00:00Z".parse().unwrap()
    }

    /// Synthetic CRD: plausible shape, not captured from a server.
    fn crontab_crd(scope: &str) -> CustomResourceDefinition {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "apiextensions.k8s.io/v1",
            "kind": "CustomResourceDefinition",
            "metadata": {"name": "crontabs.stable.example.com", "creationTimestamp": "2026-10-20T12:00:00Z"},
            "spec": {
                "group": "stable.example.com",
                "scope": scope,
                "names": {"kind": "CronTab", "plural": "crontabs", "singular": "crontab", "shortNames": ["ct", "cron"]},
                "versions": [{
                    "name": "v1", "served": true, "storage": true,
                    "additionalPrinterColumns": [
                        {"name": "Spec", "type": "string", "jsonPath": ".spec.cronSpec"},
                        {"name": "Replicas", "type": "integer", "jsonPath": ".spec.replicas"},
                        {"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"},
                        {"name": "Last Run", "type": "date", "jsonPath": ".status.lastRun"},
                        {"name": "Image", "type": "string", "jsonPath": ".spec.image", "priority": 1}
                    ]
                }]
            }
        }))
        .unwrap()
    }

    fn kind(crd: &CustomResourceDefinition) -> CustomKind {
        catalog(&[crd], None).remove(0).kinds.remove(0)
    }

    /// Synthetic custom resource (plausible shape, not captured from a server).
    fn crontab(namespace: &str, name: &str) -> DynamicObject {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "stable.example.com/v1",
            "kind": "CronTab",
            "metadata": {
                "name": name, "namespace": namespace, "uid": "uid-1",
                "creationTimestamp": "2026-10-22T11:55:00Z",
                "labels": {"app": "cron"},
                "annotations": {"note": "hi"},
                "ownerReferences": [{"apiVersion": "v1", "kind": "ConfigMap", "name": "owner", "uid": "u"}],
                "managedFields": [{"manager": "kubectl"}]
            },
            "spec": {"cronSpec": "*/5 * * * *", "replicas": 2, "image": "busybox"},
            "status": {
                "lastRun": "2026-10-22T09:00:00Z",
                "conditions": [{"type": "Ready", "status": "True", "reason": "Scheduled", "message": "ok"}]
            }
        }))
        .unwrap()
    }

    #[test]
    fn ages_follow_kubectl_human_durations() {
        let ago = |seconds: i64| now() - SignedDuration::from_secs(seconds);
        assert_eq!(format_age(ago(45), now()), "45s");
        assert_eq!(format_age(ago(5 * 60 + 30), now()), "5m30s");
        assert_eq!(format_age(ago(90 * 60), now()), "90m");
        assert_eq!(format_age(ago(5 * 3600 + 3 * 60), now()), "5h3m");
        assert_eq!(format_age(ago(30 * 3600), now()), "30h");
        assert_eq!(format_age(ago(3 * 86400 + 4 * 3600), now()), "3d4h");
        assert_eq!(format_age(ago(20 * 86400), now()), "20d");
    }

    #[test]
    fn ages_tick_every_second_below_ten_minutes_then_every_minute() {
        // Freelens 113abe5 `computeUpdateInterval`, compact: `minutes < 10`.
        let ago = |seconds: i64| now() - SignedDuration::from_secs(seconds);
        let second = std::time::Duration::from_secs(1);
        let minute = std::time::Duration::from_secs(60);
        assert_eq!(age_cadence(ago(0), now()), second);
        assert_eq!(age_cadence(ago(3 * 60 + 42), now()), second);
        assert_eq!(age_cadence(ago(10 * 60 - 1), now()), second);
        assert_eq!(age_cadence(ago(10 * 60), now()), minute);
        assert_eq!(age_cadence(ago(30 * 86400), now()), minute);
        // An object stamped ahead of this clock (skew) still counts up.
        assert_eq!(age_cadence(ago(-5), now()), second);
    }

    #[test]
    fn the_next_tick_lands_when_the_youngest_age_next_changes() {
        let ago = |millis: i64| now() - SignedDuration::from_millis(millis);
        let millis = std::time::Duration::from_millis;
        assert_eq!(next_age_change(ago(61_000), now()), millis(1_000));
        assert_eq!(next_age_change(ago(61_300), now()), millis(700));
        // 15m30s shows `15m` until 16m.
        assert_eq!(next_age_change(ago(930_000), now()), millis(30_000));
        assert_eq!(next_age_change(ago(-400), now()), millis(400));
    }

    #[test]
    fn tracking_follows_the_youngest_age_formatted_against_now() {
        let ago = |seconds: i64| now() - SignedDuration::from_secs(seconds);
        let (cells, next) = track_ages(now(), || {
            [
                format_age(ago(30 * 86400), now()),
                format_age(ago(15 * 60 + 30), now()),
                // A finished Job's duration ends before now: not a live age.
                format_age(ago(5), ago(1)),
            ]
        });
        assert_eq!(cells, ["30d", "15m", "4s"]);
        assert_eq!(next, std::time::Duration::from_secs(30));

        let (_, next) = track_ages(now(), || format_age(ago(65), now()));
        assert_eq!(next, std::time::Duration::from_secs(1));

        // A nested projection's ages count for the outer one too.
        let (_, next) = track_ages(now(), || {
            format_age(ago(30 * 86400), now());
            track_ages(now(), || format_age(ago(65), now()))
        });
        assert_eq!(next, std::time::Duration::from_secs(1));

        // Nothing aged: a minute.
        let (_, next) = track_ages(now(), || ());
        assert_eq!(next, std::time::Duration::from_secs(60));
    }

    #[test]
    fn namespaced_instances_list_priority_zero_printer_columns_between_name_and_age() {
        let crd = crontab_crd("Namespaced");
        let object = crontab("team-a", "nightly");

        let table = project_custom_resources(&kind(&crd), &[&object], now());

        assert_eq!(
            table.columns,
            ["Name", "Namespace", "Spec", "Replicas", "Last Run", "Age"]
        );
        assert_eq!(
            table.rows,
            [ResourceRow {
                key: ObjectKey::new(Some("team-a".to_string()), "nightly"),
                cells: ["nightly", "team-a", "*/5 * * * *", "2", "3h", "5m"]
                    .map(String::from)
                    .to_vec(),
            }]
        );
    }

    #[test]
    fn instances_without_printer_columns_fall_back_to_name_and_age() {
        let mut crd = crontab_crd("Cluster");
        crd.spec.versions[0].additional_printer_columns = None;
        let object = crontab("", "global");

        let table = project_custom_resources(&kind(&crd), &[&object], now());

        assert_eq!(table.columns, ["Name", "Age"]);
        assert_eq!(table.rows[0].cells, ["global", "5m"]);
    }

    #[test]
    fn definitions_list_with_group_filter() {
        let crontab = crontab_crd("Namespaced");
        let mut other = crontab_crd("Cluster");
        other.metadata.name = Some("backups.acme.io".to_string());
        other.spec.group = "acme.io".to_string();
        other.spec.names.kind = "Backup".to_string();
        other.spec.names.short_names = None;

        let all = project_definitions(&[&crontab, &other], &BTreeSet::new(), now());
        assert_eq!(
            all.columns,
            ["Resource", "Group", "Version", "ShortNames", "Scope", "Age"]
        );
        assert_eq!(
            all.rows[0].cells,
            [
                "CronTab",
                "stable.example.com",
                "v1",
                "ct, cron",
                "Namespaced",
                "2d"
            ]
        );
        assert_eq!(all.rows[1].cells[..2], ["Backup", "acme.io"]);

        let filter = BTreeSet::from(["acme.io".to_string()]);
        let filtered = project_definitions(&[&crontab, &other], &filter, now());
        assert_eq!(filtered.rows.len(), 1);
        assert_eq!(filtered.rows[0].key.to_string(), "backups.acme.io");
    }

    #[test]
    fn instance_detail_summarizes_metadata_status_and_yaml() {
        let crd = crontab_crd("Namespaced");
        let object = crontab("team-a", "nightly");

        let detail = custom_resource_detail(&kind(&crd), &object, now());

        assert_eq!(detail.title, "CronTab team-a/nightly");
        assert_eq!(
            detail.metadata,
            [
                ("Name", "nightly"),
                ("Namespace", "team-a"),
                ("UID", "uid-1"),
                ("Created", "2026-10-22T11:55:00Z (5m ago)"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert_eq!(detail.labels, ["app=cron"]);
        assert_eq!(detail.annotations, ["note=hi"]);
        assert_eq!(detail.owners, ["ConfigMap owner"]);
        assert_eq!(
            detail.status,
            [("Image".to_string(), "busybox".to_string())]
        );
        assert_eq!(detail.conditions.len(), 1);
        assert_eq!(detail.conditions[0].kind, "Ready");
        assert_eq!(detail.conditions[0].status, "True");
        assert_eq!(detail.conditions[0].reason, "Scheduled");
        assert_eq!(detail.conditions[0].message, "ok");
        assert!(
            detail.yaml.contains("cronSpec: '*/5 * * * *'")
                || detail.yaml.contains("cronSpec: \"*/5 * * * *\"")
        );
        assert!(!detail.yaml.contains("managedFields"));
    }

    #[test]
    fn definition_detail_lists_versions_scope_names_and_columns() {
        let crd = crontab_crd("Namespaced");

        let detail = definition_detail(&crd, now());

        assert_eq!(
            detail.title,
            "CustomResourceDefinition crontabs.stable.example.com"
        );
        assert_eq!(
            detail.status,
            [
                ("Versions", "v1 (served, storage)"),
                ("Scope", "Namespaced"),
                ("Kind", "CronTab"),
                ("Plural", "crontabs"),
                ("Singular", "crontab"),
                ("Short names", "ct, cron"),
                ("Printer columns", "Spec, Replicas, Age, Last Run, Image"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert!(detail.yaml.contains("group: stable.example.com"));
    }
}
