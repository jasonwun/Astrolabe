//! The Discovery cache (#16): the last successful Discovery result and
//! the failure of the latest refresh.

use astrolabe_kube::cluster::ClusterError;
use astrolabe_kube::discovery::ServedResources;

/// Last successful discovery plus the failure of the latest refresh, if it
/// failed: a failed refresh never discards a previous result.
#[derive(Debug, Default)]
pub struct DiscoveryCache {
    served: Option<ServedResources>,
    failure: Option<ClusterError>,
}

impl DiscoveryCache {
    pub fn record(&mut self, result: Result<ServedResources, ClusterError>) {
        match result {
            Ok(served) => {
                self.served = Some(served);
                self.failure = None;
            }
            Err(error) => self.failure = Some(error),
        }
    }

    pub fn served(&self) -> Option<&ServedResources> {
        self.served.as_ref()
    }

    pub fn failure(&self) -> Option<&ClusterError> {
        self.failure.as_ref()
    }
}
