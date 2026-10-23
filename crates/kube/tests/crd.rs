use astrolabe_kube::crd::{KindResource, catalog};
use astrolabe_kube::discovery::{ServedResource, ServedResources};
use astrolabe_kube::k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use astrolabe_kube::kube::discovery::ApiResource;

/// Synthetic CRD manifest: the shape `kubectl apply` accepts, not a capture.
fn crd(group: &str, kind: &str, scope: &str, versions: &str) -> CustomResourceDefinition {
    let plural = format!("{}s", kind.to_lowercase());
    serde_json::from_str(&format!(
        r#"{{"apiVersion":"apiextensions.k8s.io/v1","kind":"CustomResourceDefinition",
            "metadata":{{"name":"{plural}.{group}"}},
            "spec":{{"group":"{group}","scope":"{scope}",
              "names":{{"kind":"{kind}","plural":"{plural}","shortNames":["ct"]}},
              "versions":{versions}}}}}"#
    ))
    .unwrap()
}

fn served(group: &str, version: &str, kind: &str, namespaced: bool) -> ServedResource {
    ServedResource {
        resource: ApiResource {
            group: group.to_string(),
            version: version.to_string(),
            api_version: format!("{group}/{version}"),
            kind: kind.to_string(),
            plural: format!("{}s", kind.to_lowercase()),
        },
        namespaced,
    }
}

const ONE_VERSION: &str = r#"[{"name":"v1","served":true,"storage":true,
    "additionalPrinterColumns":[{"name":"Spec","type":"string","jsonPath":".spec.cronSpec"}]}]"#;

#[test]
fn kinds_group_by_api_group_in_alphabetical_order() {
    let crontab = crd("stable.example.com", "CronTab", "Namespaced", ONE_VERSION);
    let backup = crd("acme.io", "Backup", "Cluster", ONE_VERSION);
    let archive = crd("acme.io", "Archive", "Namespaced", ONE_VERSION);

    let groups = catalog(&[&crontab, &backup, &archive], None);

    let shape: Vec<(&str, Vec<&str>)> = groups
        .iter()
        .map(|group| {
            (
                group.group.as_str(),
                group.kinds.iter().map(|kind| kind.kind.as_str()).collect(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("acme.io", vec!["Archive", "Backup"]),
            ("stable.example.com", vec!["CronTab"]),
        ]
    );
    let backup = &groups[0].kinds[1];
    assert!(!backup.namespaced);
    assert_eq!(backup.crd_name, "backups.acme.io");
    assert_eq!(backup.plural, "backups");
    assert!(groups[0].kinds[0].namespaced);
}

#[test]
fn preferred_version_is_the_storage_version_with_its_printer_columns() {
    let versions = r#"[
        {"name":"v1beta1","served":true,"storage":false,
         "additionalPrinterColumns":[{"name":"Old","type":"string","jsonPath":".spec.old"}]},
        {"name":"v1","served":true,"storage":true,
         "additionalPrinterColumns":[{"name":"Spec","type":"string","jsonPath":".spec.cronSpec"}]}
    ]"#;
    let crontab = crd("stable.example.com", "CronTab", "Namespaced", versions);

    let groups = catalog(&[&crontab], None);

    let kind = &groups[0].kinds[0];
    assert_eq!(kind.version, "v1");
    let columns: Vec<&str> = kind.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(columns, ["Spec"]);
}

#[test]
fn kinds_resolve_against_discovery_or_report_why_not() {
    let crontab = crd("stable.example.com", "CronTab", "Namespaced", ONE_VERSION);
    let unserved_storage = crd(
        "acme.io",
        "Backup",
        "Cluster",
        r#"[{"name":"v2","served":true,"storage":false},
            {"name":"v1","served":false,"storage":true}]"#,
    );
    let pending = crd("acme.io", "Archive", "Namespaced", ONE_VERSION);

    let before_discovery = catalog(&[&crontab], None);
    assert_eq!(
        before_discovery[0].kinds[0].resource,
        KindResource::Undiscovered
    );

    let discovered = ServedResources::new([
        served("stable.example.com", "v1", "CronTab", true),
        served("acme.io", "v2", "Backup", false),
    ]);
    let groups = catalog(&[&crontab, &unserved_storage, &pending], Some(&discovered));

    let crontab = &groups[1].kinds[0];
    assert_eq!(
        crontab.resource,
        KindResource::Served(
            discovered
                .resolve("stable.example.com", "v1", "CronTab")
                .unwrap()
                .resource
                .clone()
        )
    );
    // Archive: CRD says served but discovery has not seen it (not yet
    // established); Backup: the storage version itself is not served.
    let archive = &groups[0].kinds[0];
    assert_eq!(archive.resource, KindResource::NotServed);
    let backup = &groups[0].kinds[1];
    assert_eq!(backup.version, "v1");
    assert_eq!(backup.resource, KindResource::NotServed);
}

#[test]
fn an_unserved_version_is_not_served_even_while_discovery_lags() {
    // The CRD was just switched to `served: false`; the cached discovery
    // still lists the version until the debounced refresh lands.
    let crontab = crd(
        "stable.example.com",
        "CronTab",
        "Namespaced",
        r#"[{"name":"v1","served":false,"storage":true}]"#,
    );
    let stale = ServedResources::new([served("stable.example.com", "v1", "CronTab", true)]);

    let groups = catalog(&[&crontab], Some(&stale));

    assert_eq!(groups[0].kinds[0].resource, KindResource::NotServed);
}
