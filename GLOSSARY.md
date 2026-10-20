# Astrolabe

A native GPUI/Rust rewrite of the Freelens Kubernetes desktop client. This glossary pins the project's domain language as it crystallizes.

## Language

**Cluster Session**:
The live connection to one cluster together with everything opened under it; a session opens when the cluster is selected in the sidebar, and several sessions may coexist.
_Avoid_: cluster connection, context

**Resource Registry**:
The declarative table of resource kinds the app knows; it drives which watchers exist and how each kind is presented.
_Avoid_: resource list, kind config

**Row Projection**:
The per-kind mapping from a stored object to one row of the list view; hand-written for built-in kinds, derived from `additionalPrinterColumns` for CRDs.
_Avoid_: column mapping, table adapter

**Relist**:
The full re-list and watch rebuild forced when the apiserver rejects a stale resourceVersion with 410 Gone.

**Stale**:
The state of a list that keeps showing its last known data while its watch is down or relisting; surfaced to the user, never blanked.
_Avoid_: loading, disconnected
