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

Pin a commit, then fetch raw files into the scratchpad:

```sh
SHA=$(gh api repos/freelensapp/freelens/commits/main --jq .sha)
curl -sfL "https://raw.githubusercontent.com/freelensapp/freelens/$SHA/packages/core/src/renderer/components/network-services/services.tsx"
```

Cite the pinned commit in the module doc of the projection that follows
it (`Freelens 088d085, network-services/`), so a later reader can diff
against what was copied.

## Typed projections

Check the pinned `k8s-openapi` type against every upstream field before
projecting from it: deserialization discards fields the typed model does
not know. For example, `Service` loses `topologyKeys` and load-balancer
conditions. Preserve such fields from `DynamicObject` or the registration's
original JSON rather than a typed round-trip. Test them through the real
registration/UI path with a fixture that fails if the field passes only
through the typed model.

## What follows upstream, what doesn't

- **Follows upstream:** which columns and fields exist, their order and
  titles, and how each value is derived.
- **Astrolabe's own:** widget styling (badges, tooltips, row truncation).
- **Accepted deviations:** built-in kind lists leave out the Namespace
  column (#17). Any other deviation is a question for the user before it
  is implemented, not a decision recorded afterwards.
