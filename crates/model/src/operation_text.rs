//! An Operation's on-screen text: what a detail tab's toolbar shows while
//! it runs, once the server accepts it, and when it fails. The text lives
//! here, not in the cluster layer, which holds no UI wording (#147).

use astrolabe_kube::object::Operation;

/// Shown while the server has not answered.
pub fn pending(operation: &Operation) -> &'static str {
    match operation {
        Operation::Scale { .. } => "Scaling…",
        Operation::RolloutRestart { .. } => "Restarting…",
        Operation::SetSuspend { suspend: true, .. } => "Suspending…",
        Operation::SetSuspend { suspend: false, .. } => "Resuming…",
        Operation::TriggerCronJob { .. } => "Triggering…",
        Operation::Delete { .. } => "Deleting…",
    }
}

/// Shown once the server accepted it.
pub fn done(operation: &Operation) -> String {
    match operation {
        Operation::Scale { replicas, .. } => format!("Scaled to {replicas}"),
        Operation::RolloutRestart { .. } => "Restart requested".to_string(),
        Operation::SetSuspend { suspend: true, .. } => "Suspended".to_string(),
        Operation::SetSuspend { suspend: false, .. } => "Resumed".to_string(),
        Operation::TriggerCronJob { .. } => "Job creation requested".to_string(),
        Operation::Delete { .. } => "Deletion requested".to_string(),
    }
}

/// What failed, as in "{verb} failed: {error}".
pub fn verb(operation: &Operation) -> &'static str {
    match operation {
        Operation::Scale { .. } => "Scale",
        Operation::RolloutRestart { .. } => "Restart",
        Operation::SetSuspend { suspend: true, .. } => "Suspend",
        Operation::SetSuspend { suspend: false, .. } => "Resume",
        Operation::TriggerCronJob { .. } => "Trigger",
        Operation::Delete { .. } => "Delete",
    }
}
