# Astrolabe

A native desktop Kubernetes client (a Rust/GPUI rewrite of Freelens): live views of clusters, their resources, and edits applied back to them.

## Clusters and watching

**Cluster**:
One kubeconfig context the user can open.
_Avoid_: context (in UI and issue text), connection

**Session**:
An opened cluster in the app, owning that cluster's watches and their stores for as long as it is open; several sessions coexist.
_Avoid_: connection, tab

**Watch**:
A long-lived list-then-stream of one kind's objects, identified by its watch key.
_Avoid_: subscription, poller

**Watch key**:
The identity of a watch: cluster, kind, and namespace, where no namespace means a cluster-scoped watch.
_Avoid_: watcher id

**Snapshot**:
The complete set of objects a watch last listed, replaced whole, never patched mid-relist.

**Relist**:
A fresh complete LIST after a watch fails, ending in a new snapshot.
_Avoid_: resync, reconnect (a reconnect is not a relist)

**Stale**:
Showing the previous snapshot while its watch is down or relisting.
_Avoid_: outdated, offline

## Views

**Registration**:
What a built-in kind plugs into the generic list and detail: its list columns, its status summary, its actions and, optionally, a related section. Built-in kinds have no hand-written views.
_Avoid_: custom view, plugin

**Related section**:
A detail tab's list of other objects tied to the shown one (a Pod's Events, a Deployment's Pods, a Service's EndpointSlices), read from their own watches. Empty, it says so, unless its kind opts to hide it (a Service with no EndpointSlices).
_Avoid_: children, sub-resources

## Custom resources

**Discovery**:
Learning which kinds, at which versions, the apiserver serves.
_Avoid_: API scan

**Served**:
A kind at a version that discovery lists and its definition marks served; otherwise **not served**.
_Avoid_: available, enabled

**Definition**:
A CustomResourceDefinition, the object that declares a custom kind.
_Avoid_: CRD type, schema

**Preferred version**:
The version of a custom kind Astrolabe lists and shows: the definition's storage version, which may itself be not served.
_Avoid_: default version, latest version

**Catalog**:
The custom kinds of a cluster grouped by API group, each pinned to its preferred version; what Custom Resources navigation shows.
_Avoid_: CRD list (that is the Definitions list)

**Printer column**:
A column a definition declares for its instance list; priority-0 columns go in the list, higher-priority ones in the detail status summary.
_Avoid_: additional column

## Editing

**Apply**:
Sending an edited manifest to the cluster as a server-side apply.
_Avoid_: save, update, patch

**Precheck**:
A dry-run apply that reports problems without changing the cluster.
_Avoid_: validation, lint
