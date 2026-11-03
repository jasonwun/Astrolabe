//! The Discovery cache, fed Discovery results built by hand.

use astrolabe_kube::cluster::ClusterError;
use astrolabe_kube::discovery::{ServedResource, ServedResources};
use astrolabe_kube::kube::discovery::ApiResource;
use astrolabe_model::discovery_cache::DiscoveryCache;

/// A Discovery result that serves `stable.example.com/v1` `CronTab`.
fn crontab_served() -> ServedResources {
    ServedResources::new([ServedResource {
        resource: ApiResource {
            group: "stable.example.com".to_string(),
            version: "v1".to_string(),
            api_version: "stable.example.com/v1".to_string(),
            kind: "CronTab".to_string(),
            plural: "crontabs".to_string(),
        },
        namespaced: true,
    }])
}

#[test]
fn failed_refresh_keeps_the_previous_result() {
    let first = Ok(crontab_served());
    let mut cache = DiscoveryCache::default();
    assert!(cache.served().is_none());

    cache.record(first);
    assert!(cache.failure().is_none());

    cache.record(Err(ClusterError::new("connection refused")));
    assert_eq!(
        cache.failure(),
        Some(&ClusterError::new("connection refused"))
    );
    assert!(
        cache
            .served()
            .and_then(|served| served.resolve("stable.example.com", "v1", "CronTab"))
            .is_some()
    );

    cache.record(Ok(crontab_served()));
    assert!(cache.failure().is_none());
}
