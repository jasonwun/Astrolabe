# Three layers; only Session is bound to the GPUI main thread

The app is split into three layers so that each layer's behaviour can be tested and debugged through that layer's own interface, without starting the layers above it.

| Layer | Crate | Holds | May depend on |
|---|---|---|---|
| UI | `astrolabe` (`crates/app`), and `astrolabe-terminal` as a UI-layer crate | GPUI views, the Workspace, the Session entity | model layer, cluster layer (only as below), gpui |
| Model | `astrolabe-model`, exactly one crate | the snapshot and Stale state of each watch (`Watched`), the Custom Resources Catalog, Registrations and their projections, detail resolution, and Session's other rules: when a CRD change triggers rediscovery (the debounce) and the Forward list's states | cluster layer, for value types only |
| Cluster | `astrolabe-kube` | all cluster I/O: connecting, watches with their relist and backoff, operations on objects, logs, exec, port-forward, Apply and Precheck; the `Operation` vocabulary | kube-rs, k8s-openapi, tokio |

The rules for each layer are as follows.

- **The model layer is synchronous and does no I/O.** It defines no `async fn`. It never names `kube::Client`, the cluster layer's connection handle, or any function of the cluster layer that performs I/O. It may use the cluster layer's value types: Kubernetes objects, `DynamicObject`, `ApiResource`, object addresses, and watch events. It does not depend on gpui or gpui-kit, `SharedString` included, and it does not read the clock. Time is passed in as an argument, as projections already take `now`.
- **The UI layer reaches the cluster only through Session.** Session holds the cluster I/O seam (`ClusterIo`), and views ask Session. No view holds a `kube::Client` or a connection handle, and no view calls a cluster layer function that performs I/O. Views may name the cluster layer's value types.
- **Session holds no rules of its own.** It owns the model-layer state for its Cluster and the `ClusterIo`. It runs the drain loops that feed watch messages into that state, owns the timers and tasks, and calls `cx.notify()`. Any decision about that data that can be made without a `Context` belongs in the model layer: what a snapshot is, when a watch counts as Stale, what a list or detail shows. Session only carries it out.
- **Each behaviour is tested in the lowest layer that owns it.** Cluster layer tests use a mock `kube::Client`. Model layer tests are plain `#[test]` with no runtime. UI tests use `gpui_kit::test` over a scripted cluster, and they test rendering and interaction, not rules from lower layers.

## What changes from #8/#9

#9 kept the store and the Session entity in the app crate. Its reason was that splitting them out "only creates a meaningless trait boundary" over gpui. That held while the store was written inside the entity. Since #105, `Watched` uses neither gpui nor `cx`, so it moves to the model layer as a plain struct and no trait is needed.

What stays from #8/#9:

- Session is a main-thread GPUI entity.
- gpui is never put behind a trait in order to move Session, or any other entity, out of the UI layer.

## Choosing an enum or a trait at a seam

#42's `ClusterIo` comment read the #9 note as "seams are enums, not traits". That reading is retired. The question for a seam is who adds its adapters, not which crates the adapters' implementations live in.

- **Use an enum** when the crate that defines the seam knows every adapter, and no other crate needs to add one. The variants may wrap types from other crates.
- **Use a trait** when a crate other than the one defining the seam must add adapters.

The two seams fall under this rule as follows:

- **Cluster I/O.** The app crate defines `ClusterIo = Live | Scripted`, an enum. `Live` wraps the cluster layer's connection handle, and `Scripted` is the app's test adapter.
- **Cluster connection.** `astrolabe-kube` owns it. Kubeconfig contexts are the only source today, so there is no enum yet: the layers above name a Cluster only through an opaque identity type that `astrolabe-kube` defines. A second source, such as AKS listed through Azure, becomes an enum inside that type and inside `astrolabe-kube`, and the layers above do not change.

The scripted adapter for `ClusterIo` ships in the app binary. This acceptance covers that one adapter. It does not allow other test-only code into the binary.

## Considered options

- **Two layers (UI and backend).** Rejected because the projections belong to neither layer. In the backend they would sit next to cluster I/O. In the UI their tests would compile against gpui, and nothing would stop view code from mixing in.
- **A trait for the cluster I/O seam, with the scripted adapter only in test support.** Rejected. Once the live implementation sits in `astrolabe-kube`, each enum arm is one line. Revisit this if a crate other than the app crate needs to add a cluster I/O adapter.
- **Keep watch state inside Session.** Rejected. Relist behaviour would then be testable only through GPUI, which today means a GPUI test that runs a hand-written TCP apiserver.

## Consequences

- The "workspace decision" comments in `crates/app/src/lib.rs` and `crates/app/src/cluster_io.rs` are rewritten to cite this ADR.
- Moving Registrations into the model layer removes their gpui-kit `SharedString` (`builtin.rs`).
- Detail resolution (`detail.rs`) takes model-layer state instead of `&Session`.
- The Catalog (`crd::catalog`, `preferred_version`, `KindResource`) moves from `astrolabe-kube` to `astrolabe-model`. Discovery's requests stay in `astrolabe-kube`.
- The rediscovery debounce's generation rule and the Forward list's states move to `astrolabe-model`. Session keeps only the 400 ms timer and the forward tasks.
- The layer rules above that a script can check are checked in CI: the model layer's direct dependencies are an allowlist without gpui, gpui-kit, tokio or futures, and the model and UI layers are searched for the names they must not use.
- Adding a Cluster source changes the cluster layer only. Any GLOSSARY change to **Cluster** ("one kubeconfig context") is made in the change that adds the source, not by this ADR.
