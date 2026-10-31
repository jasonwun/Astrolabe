//! P3 built-in workload projections, aligned with Freelens 14a491f.

use astrolabe::projection::ReplicaCounts;
use astrolabe::projection::workloads::{
    cronjob_detail, daemonset_detail, job_detail, project_cronjobs, project_daemonsets,
    project_jobs, project_replicasets, project_statefulsets, replicaset_detail, statefulset_detail,
};
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

fn now() -> Timestamp {
    "2026-10-22T12:00:00Z".parse().unwrap()
}

#[test]
fn statefulsets_list_and_detail_follow_freelens() {
    let statefulset = serde_json::from_value(serde_json::json!({
        "apiVersion": "apps/v1", "kind": "StatefulSet",
        "metadata": {"name": "db", "namespace": "default", "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {
            "replicas": 3, "serviceName": "db", "selector": {"matchLabels": {"app": "db"}},
            "template": {"metadata": {"labels": {"app": "db"}}, "spec": {
                "nodeSelector": {"disk": "ssd"},
                "containers": [{"name": "db", "image": "postgres:17"}],
                "tolerations": [{"key": "dedicated", "operator": "Equal", "value": "db", "effect": "NoSchedule"}],
                "affinity": {"nodeAffinity": {}, "podAffinity": {}}
            }}
        },
        "status": {"replicas": 3, "readyReplicas": 2, "updatedReplicas": 2, "availableReplicas": 2, "unavailableReplicas": 1}
    }))
    .unwrap();

    let table = project_statefulsets(&[&statefulset], now());
    assert_eq!(table.headers(), ["Name", "Ready", "Desired", "Age"]);
    assert_eq!(table.rows[0].cells, ["db", "2", "3", "60m"]);

    let detail = statefulset_detail(&statefulset, now());
    assert_eq!(detail.title, "StatefulSet default/db");
    assert_eq!(
        detail.replicas,
        Some(ReplicaCounts {
            desired: 3,
            updated: 2,
            available: 2,
            unavailable: 1
        })
    );
    assert_eq!(
        detail.status,
        [
            ("Selector", "app=db"),
            ("Node Selector", "disk=ssd"),
            ("Images", "postgres:17"),
            ("Tolerations", "dedicated=db:NoSchedule"),
            ("Affinities", "2"),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string()))
    );
}

#[test]
fn replicasets_list_and_detail_follow_freelens() {
    let replicaset = serde_json::from_value(serde_json::json!({
        "apiVersion": "apps/v1", "kind": "ReplicaSet",
        "metadata": {"name": "web-abc", "namespace": "default", "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {"replicas": 3, "selector": {"matchLabels": {"app": "web"}},
                 "template": {"spec": {"containers": [{"name": "web", "image": "nginx:1.27"}]}}},
        "status": {"replicas": 3, "availableReplicas": 2, "readyReplicas": 2}
    }))
    .unwrap();
    let table = project_replicasets(&[&replicaset], now());
    assert_eq!(
        table.headers(),
        ["Name", "Desired", "Current", "Ready", "Age"]
    );
    assert_eq!(table.rows[0].cells, ["web-abc", "3", "2", "2", "60m"]);
    let detail = replicaset_detail(&replicaset, now());
    assert_eq!(
        detail.status,
        [
            ("Selector", "app=web"),
            ("Images", "nginx:1.27"),
            ("Replicas", "2 current / 3 desired"),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string()))
    );
}

#[test]
fn daemonsets_list_and_detail_follow_freelens() {
    let daemonset = serde_json::from_value(serde_json::json!({
        "apiVersion": "apps/v1", "kind": "DaemonSet",
        "metadata": {"name": "agent", "namespace": "default", "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {"selector": {"matchLabels": {"app": "agent"}},
                 "updateStrategy": {"type": "RollingUpdate"},
                 "template": {"spec": {"nodeSelector": {"role": "worker"},
                                       "initContainers": [{"name": "init", "image": "init:1"}],
                                       "containers": [{"name": "agent", "image": "agent:2"}]}}},
        "status": {"desiredNumberScheduled": 4, "currentNumberScheduled": 4,
                   "numberReady": 3, "updatedNumberScheduled": 2, "numberAvailable": 3,
                   "numberUnavailable": 1, "numberMisscheduled": 0}
    }))
    .unwrap();
    let table = project_daemonsets(&[&daemonset], now());
    assert_eq!(
        table.headers(),
        [
            "Name",
            "Desired",
            "Current",
            "Ready",
            "Updated",
            "Available",
            "Node Selector",
            "Age"
        ]
    );
    assert_eq!(
        table.rows[0].cells,
        ["agent", "4", "4", "3", "2", "3", "role=worker", "60m"]
    );
    let detail = daemonset_detail(&daemonset, now());
    assert_eq!(
        detail.status,
        [
            ("Selector", "app=agent"),
            ("Node Selector", "role=worker"),
            ("Images", "agent:2,init:1"),
            ("Strategy Type", "RollingUpdate"),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string()))
    );
    assert_eq!(
        detail.replicas,
        Some(ReplicaCounts {
            desired: 4,
            updated: 2,
            available: 3,
            unavailable: 1
        })
    );
}

#[test]
fn jobs_list_and_detail_follow_freelens_completion_status() {
    let job = serde_json::from_value(serde_json::json!({
        "apiVersion": "batch/v1", "kind": "Job",
        "metadata": {"name": "migrate", "namespace": "default", "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {"parallelism": 2, "completions": 4, "completionMode": "Indexed",
                 "backoffLimit": 3, "ttlSecondsAfterFinished": 600, "activeDeadlineSeconds": 120, "suspend": false,
                 "selector": {"matchLabels": {"job": "migrate"}},
                 "template": {"spec": {
                     "containers": [{"name": "migrate", "image": "busybox"}],
                     "tolerations": [{"key": "batch", "operator": "Exists"}],
                     "affinity": {"nodeAffinity": {}, "podAntiAffinity": {}}
                 }}},
        "status": {"active": 0, "ready": 0, "succeeded": 4, "failed": 1,
                   "startTime": "2026-10-22T11:50:00Z", "completionTime": "2026-10-22T11:55:00Z",
                   "completedIndexes": "0-3", "conditions": [{"type": "Complete", "status": "True"}]}
    }))
    .unwrap();
    let table = project_jobs(&[&job], now());
    assert_eq!(
        table.headers(),
        [
            "Name",
            "Resumed",
            "Status",
            "Succeeded",
            "Completions",
            "Parallelism",
            "Duration",
            "Age"
        ]
    );
    assert_eq!(
        table.rows[0].cells,
        ["migrate", "true", "Complete", "4", "4", "2", "5m", "60m"]
    );
    let detail = job_detail(&job, now());
    assert_eq!(
        detail.status,
        [
            ("Selector", "job=migrate"),
            ("Status", "Complete"),
            ("Parallelism", "2"),
            ("Completions", "4"),
            ("Completion Mode", "Indexed"),
            ("Resumed", "true"),
            ("Backoff Limit", "3"),
            ("TTL Seconds After Finished", "10m"),
            ("Start Time", "2026-10-22T11:50:00Z (10m ago)"),
            ("Completed At", "2026-10-22T11:55:00Z (5m ago)"),
            ("Duration", "5m"),
            ("Active Deadline Seconds", "2m"),
            (
                "Pods Statuses",
                "0 Active (0 Ready) / 4 Succeeded / 1 Failed"
            ),
            ("Completed Indexes", "0-3"),
            ("Tolerations", "batch"),
            ("Affinities", "2"),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string()))
    );
    assert_eq!(detail.conditions[0].kind, "Complete");
}

#[test]
fn cronjobs_list_and_detail_follow_freelens_schedule_status() {
    let cronjob = serde_json::from_value(serde_json::json!({
        "apiVersion": "batch/v1", "kind": "CronJob",
        "metadata": {"name": "nightly", "namespace": "default", "creationTimestamp": "2026-10-22T11:00:00Z"},
        "spec": {"schedule": "0 0 * * *", "timeZone": "UTC", "suspend": false,
                 "startingDeadlineSeconds": 300, "concurrencyPolicy": "Forbid",
                 "successfulJobsHistoryLimit": 3, "failedJobsHistoryLimit": 1,
                 "jobTemplate": {"spec": {"parallelism": 1, "completions": 1,
                                          "backoffLimit": 4, "ttlSecondsAfterFinished": 600,
                                          "template": {"spec": {"containers": [{"name": "job", "image": "busybox"}]}}}}},
        "status": {"active": [{"kind": "Job", "name": "nightly-1", "namespace": "default", "uid": "job-1", "apiVersion": "batch/v1"}],
                   "lastScheduleTime": "2026-10-22T11:58:00Z", "lastSuccessfulTime": "2026-10-22T11:57:00Z"}
    }))
    .unwrap();
    let table = project_cronjobs(&[&cronjob], now());
    assert_eq!(
        table.headers(),
        [
            "Name",
            "Schedule",
            "Timezone",
            "Resumed",
            "Active",
            "Last schedule",
            "Age"
        ]
    );
    assert_eq!(
        table.rows[0].cells,
        ["nightly", "0 0 * * *", "UTC", "true", "1", "2m", "60m"]
    );
    let detail = cronjob_detail(&cronjob, now());
    assert_eq!(
        detail.status,
        [
            ("Schedule", "0 0 * * * (At 12:00 AM)"),
            ("Timezone", "UTC"),
            ("Starting Deadline Seconds", "5m"),
            ("Concurrency Policy", "Forbid"),
            ("Resumed", "true"),
            ("Successful Jobs History Limit", "3"),
            ("Failed Jobs History Limit", "1"),
            ("Last Schedule", "2026-10-22T11:58:00Z (2m ago)"),
            ("Last Successful Run", "2026-10-22T11:57:00Z (3m ago)"),
            ("Active", "1"),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string()))
    );
    assert_eq!(detail.sections[0].title, "Template");
    let fields = detail.sections[0]
        .items
        .iter()
        .map(|item| {
            let astrolabe::projection::DetailItem::Field(field) = item else {
                panic!("template field expected")
            };
            (field.label.as_str(), field.values[0].text.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        fields,
        [
            ("Parallelism", "1"),
            ("Completions", "1"),
            ("Resumed", "true"),
            ("Backoff Limit", "4"),
            ("TTL Seconds After Finished", "10m"),
        ]
    );
}

#[test]
fn a_zero_second_ttl_reads_zero_seconds() {
    let cronjob = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "once", "namespace": "default"},
        "spec": {"schedule": "@yearly", "jobTemplate": {"spec": {
            "ttlSecondsAfterFinished": 0,
            "template": {"spec": {"containers": [], "restartPolicy": "Never"}}
        }}}
    }))
    .unwrap();
    let detail = cronjob_detail(&cronjob, now());
    let ttl = detail.sections[0].items.iter().find_map(|item| {
        let astrolabe::projection::DetailItem::Field(field) = item else {
            return None;
        };
        (field.label == "TTL Seconds After Finished").then(|| field.values[0].text.as_str())
    });
    assert_eq!(ttl, Some("0s"));
    assert_eq!(
        detail.status[0],
        (
            "Schedule".to_string(),
            "@yearly (At 12:00 AM, on day 1 of the month, only in January)".to_string()
        )
    );
    assert_eq!(detail.status[1], ("Timezone".to_string(), String::new()));
}

#[test]
fn schedule_descriptions_cover_steps_weekdays_and_invalid_syntax() {
    let detail = |schedule: &str| {
        let cronjob = serde_json::from_value(serde_json::json!({
            "metadata": {"name": "scheduled", "namespace": "default"},
            "spec": {"schedule": schedule, "jobTemplate": {"spec": {
                "template": {"spec": {"containers": [], "restartPolicy": "Never"}}
            }}}
        }))
        .unwrap();
        cronjob_detail(&cronjob, now()).status.remove(0).1
    };
    assert_eq!(detail("*/15 * * * *"), "*/15 * * * * (Every 15 minutes)");
    assert_eq!(
        detail("0 9 * * 1-5"),
        "0 9 * * 1-5 (At 09:00 AM, Monday through Friday)"
    );
    assert_eq!(
        detail("not cron"),
        "not cron (Unrecognized cron expression syntax)"
    );
}

#[test]
fn empty_template_fields_stay_distinct_from_empty_items() {
    let statefulset = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "empty", "namespace": "default"},
        "spec": {"serviceName": "empty", "selector": {}, "template": {"spec": {
            "containers": [{"name": "empty", "image": ""}], "affinity": {}
        }}}
    }))
    .unwrap();
    let detail = statefulset_detail(&statefulset, now());
    assert_eq!(detail.status, [("Images".to_string(), String::new())]);
}

#[test]
fn a_cronjob_detail_says_whether_it_is_suspended() {
    let suspended = |spec: serde_json::Value| {
        let cronjob = serde_json::from_value(serde_json::json!({
            "metadata": {"name": "nightly", "namespace": "default"},
            "spec": spec
        }))
        .unwrap();
        cronjob_detail(&cronjob, now()).suspended
    };
    let template = serde_json::json!({"spec": {
        "template": {"spec": {"containers": [], "restartPolicy": "Never"}}
    }});
    assert_eq!(
        suspended(
            serde_json::json!({"schedule": "@daily", "suspend": true, "jobTemplate": template})
        ),
        Some(true)
    );
    assert_eq!(
        suspended(serde_json::json!({"schedule": "@daily", "jobTemplate": template})),
        Some(false)
    );
}
