# Reference product: Freelens

Astrolabe rewrites Freelens (roadmap #1). When an issue says "对照 Lens",
"align with Lens", or otherwise names upstream columns, fields, or
behaviour, the source of truth is **Freelens's source on `main`**, read
before any column or field is written:

- Repo: <https://github.com/freelensapp/freelens>, branch `main`.
- `lensapp/lens` is the wrong source: its open-source `master` froze in
  August 2023 and has since drifted from Freelens (e.g. Freelens dropped
  the Services `Selector` and Secrets `Labels` columns).

## Where things live

| What | Path under `packages/` |
| --- | --- |
| List columns and cells (`renderTableHeader` / `renderTableContents`) | `core/src/renderer/components/<dir>/<kinds>.tsx` |
| Detail panel fields and tables | `core/src/renderer/components/<dir>/<kind>-details.tsx` |
| Derived values (`getStatus`, `getPorts`, `toString`, …) | `kube-object/src/specifics/<kind>.ts` |

`<dir>` is the component folder for the kind, e.g. `workloads-pods`,
`network-services`, `network-ingresses`, `config-maps`,
`config-secrets`. List the folder names with
`gh api repos/freelensapp/freelens/contents/packages/core/src/renderer/components --jq '.[].name'`.

## Fetching

Pin a commit, then fetch raw files into the ignored scratch directory. Read
or search the saved files for the functions you need instead of printing
whole components into the conversation:

```sh
SHA=$(gh api repos/freelensapp/freelens/commits/main --jq .sha)
DEST=.scratch/freelens/$SHA
mkdir -p "$DEST"
curl -sfL "https://raw.githubusercontent.com/freelensapp/freelens/$SHA/packages/core/src/renderer/components/network-services/services.tsx" \
  -o "$DEST/services.tsx"
```

Cite the pinned commit in the module doc of the projection that follows
it (`Freelens 088d085, network-services/`), so a later reader can diff
against what was copied.

## Field inventory

Before writing a projection, list every upstream column and detail field in
render order. For each one record the source field, presence condition,
derived-value function, and how missing values, empty strings, empty
collections and collections containing an empty item render. Map each entry
to a projection or UI assertion. This makes omitted fields and order changes
visible before implementation.

Inspect suspicious upstream control flow instead of copying it blindly. A
presence predicate that hides a populated field, a constant that should be a
count, or an order contradicted by the source values needs an explicit
semantic check against the Kubernetes fields and, when it changes the agreed
contract, the deviation question below.

## Typed projections

Check the pinned `k8s-openapi` type against every upstream field before
projecting from it: deserialization discards fields the typed model does
not know. For example, `Service` loses `topologyKeys` and load-balancer
conditions. Preserve such fields from `DynamicObject.data` rather than a
typed round-trip; `data` holds the body minus `metadata` and
`apiVersion`/`kind`. Test them through the real registration/UI path with
a fixture that fails if the field passes only through the typed model.

## What follows upstream, what doesn't

- **Follows upstream:** which columns and fields exist, their order and
  titles, and how each value is derived. Sidebar order follows each
  kind's `*sidebar-item.injectable.tsx` `orderNumber`, even where an
  issue lists the kinds in another order (#20).
- **Astrolabe's own:** widget styling (badges, tooltips, row truncation).
- **Accepted deviations:** built-in kind lists leave out the Namespace
  column (#17). Any other deviation is a question for the user before it
  is implemented, not a decision recorded afterwards.
