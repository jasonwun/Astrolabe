# Manual UI check

`gpui_kit` tests assert what the element tree contains, not what a person
sees: invisible text (#52) and a tab with no close button (#51) both passed
the suite. Before handing over a change that shows in the app, look at it
on the dev cluster and attach the screenshots to the hand-over.

Read the whole file before launching, sections 0-3 included: they are how
the app is rebased, seeded, launched and driven. #233 read only the first
60 lines and improvised with `cargo run`, full-screen `screencapture` and
`osascript`. The first capture showed the terminal, not the app (the window
was behind it). A System Events click on a sidebar row did nothing, because
GPUI ignores accessibility presses. A raw CGEvent click did nothing either
until a mouse move and a short delay came first. macOS also raised a
screen-recording prompt for another app that blocked every click until
someone pressed Allow: ask the user to press it, do not press it unasked.
Use the documented tools (`dev/resize.sh`, `dev/shot.sh`,
`orca computer`) instead (#233 retro).

A change to the workspace build counts even when `crates/app` is
untouched: root `Cargo.toml`, `Cargo.lock`, workspace lints, a new
crate. It runs "A change that must leave the UI as it was" below on the
launch view and one list. #148 (an empty crate and a lint table) first
skipped the check as invisible, and the user asked for it.

A test-only change counts too. #230 (a flaky test's assertion) skipped the
check as "no UI change" and the user asked for it: drive the path the test
covers (Pods > `multi` > Port Forward > Stop, then `lsof` on the port).

A backend change counts when its output reaches a view: drive that view
through the changed path. #172 (field-path parsing in `astrolabe-kube`)
applied a Deployment with `image: ""` from the YAML editor and read the
422's line mark.

When the changed path is Exec or Port Forward (#239 moved their namespace
lookup), drive it in the app, or run its kind e2e before the PR and list
the run in Evidence: `cargo test -p astrolabe-kube --test e2e_cluster --
--ignored pod_logs_exec_and_port_forward
service_port_forward_reaches_a_selected_pod`. #239 drove only Logs by hand
and ran these two after the merge, when a failure would have been a revert
(#239 retro).

A change to which source wins when two agree on kind (#243: discovery
vs the CRD's `spec.scope`) shows the same screen on kind, because the
sources never disagree there. The screenshots then prove only that
served kinds still list and scope their columns as before; say so in the
PR and leave the disagreeing case to the model-layer test (#243 retro).

A change to a path the UI cannot reach yet (#234: a Session reconnects
after a failed connect, with no retry button) shows the normal path
unchanged on kind, the same as for #243. Say in the PR that the
screenshots prove only that, and name the test that drives the new path.
#245's eviction needs more than 16 watches, so 17 namespaces or kinds: its
screenshots showed lists and a detail with Related unchanged, and the model
and Session tests drove the eviction. #274 (a dropped reader stops its
watch) is the same: the screenshots showed Pods and Namespaces listing and a
`kubectl delete` reaching the view, and the kube tests counted the closed
apiserver requests. #275 (the detail rule's loading and failed outcomes) is the
same: a custom kind's version switch is over before a capture, and a dead
server cannot open a detail. The screenshots showed a Deployment detail with
Related Pods and the "no longer exists" text after a `kubectl delete`; the
model tests drove loading, failed and the version switch (#275 retro).
#277 (watches and Discovery retry a failed connect) is the same: kind never
fails its connect, so the screenshots showed Namespaces, Pods and a custom
kind listing after Discovery, and the kube, model and Session tests drove
the failure and the recovery (#277 retro).

A state that needs a revoked permission (#282: a list denied after its
Snapshot) needs an identity whose rights you can edit, not the admin one.
Create a ServiceAccount, a ClusterRole (`namespaces`, `pods`,
`customresourcedefinitions` get/list/watch) and a ClusterRoleBinding with
`kubectl apply -f -` (`create clusterrolebinding --serviceaccount` rejected
the namespace here), put `kubectl create token` in a copy of the kubeconfig
(`set-credentials`, then `set-context --current --user`), launch with it,
then patch the ClusterRole. An established watch ignores the change: the
banner appeared only after the watch timed out and its relist got the 403,
about 5 minutes, so `sleep 290` once instead of poking the pod. Delete the
ClusterRole and binding afterwards, they are cluster-scoped (#282 retro).

A change to which namespace a Session starts in needs a context that names
one, and no `ASTROLABE_NAMESPACE`: that variable overrides the context.
Copy the kubeconfig into the scratchpad, set the namespace on the copy and
launch with it (#236):

```sh
kubectl config view --raw > "$SCRATCH/ns.kubeconfig"
KUBECONFIG="$SCRATCH/ns.kubeconfig" kubectl config set-context kind-astrolabe-dev --namespace="$(dev/demo.sh apply)"
KUBECONFIG="$SCRATCH/ns.kubeconfig" ASTROLABE_CLUSTER=kind-astrolabe-dev cargo run -p astrolabe
```

The selector shows that namespace and Pods lists its objects. Write the
full scratchpad path in each command.

`kind-astrolabe-dev` serves aggregated discovery, so a change to the
per-group fallback never runs there. Check it there anyway, then again on
a throwaway pre-1.27 kind cluster with its own kubeconfig, and confirm
the debug log line `using per-group discovery`:

```sh
K="$SCRATCH/legacy.kubeconfig"
kind create cluster --name legacy-<issue> --image kindest/node:v1.25.16 --kubeconfig "$K" --wait 120s
kubectl --kubeconfig "$K" apply -f dev/demo-crd.yaml   # then the objects the check needs
KUBECONFIG="$K" ASTROLABE_CLUSTER=kind-legacy-<issue> ASTROLABE_NAMESPACE=<ns> \
    RUST_LOG=astrolabe_kube=debug cargo run -p astrolabe
kind delete cluster --name legacy-<issue>
```

Origin: #190 rewrote the per-group path. The `kind-astrolabe-dev`
screenshots alone would have shown only the aggregated path (#190 retro).

A change to how Discovery treats a failing group version needs a group
version that fails on the legacy node, and a wait for the skip line, not
for the first log line. An `APIService` whose Service is missing answers
503 on `/apis/<group>/<version>` and is still listed under `/apis`:

```sh
kubectl --kubeconfig "$K" apply -f - <<'YAML'
apiVersion: apiregistration.k8s.io/v1
kind: APIService
metadata: {name: v1.broken.example.com}
spec: {group: broken.example.com, version: v1, groupPriorityMinimum: 100, versionPriority: 10, service: {name: nope, namespace: default}}
YAML
```

Launch with `RUST_LOG=astrolabe_kube=debug,kube_client=warn`: the second
filter prints each `failed with status 503` line, so a stall shows as
retries instead of silence. #279's kube test, over a mock, passed in
milliseconds while the real node needed about ten minutes to reach the
skip, because kube-client retries a 503 15 times by default (#279 retro).

A state the cluster cannot produce on its own, "cannot be reached" (#276),
needs a kubeconfig copy whose server is a dead port, and no
`ASTROLABE_NAMESPACE` (the app cannot list namespaces without a server):

```sh
kubectl config view --raw --minify --context kind-astrolabe-dev \
    | sed -E 's#(server: https://[^:]+):[0-9]+#\1:1#' > "$SCRATCH/dead.kubeconfig"
KUBECONFIG="$SCRATCH/dead.kubeconfig" ASTROLABE_CLUSTER=kind-astrolabe-dev cargo run -p astrolabe
```

Each list then shows its error and the workspace banner appears, which
pushes the sidebar down about 45pt: capture before the first click and read
the new coordinates off it. A live list with no objects needs a namespace
with none of the kind, such as `kube-public`'s Pods; the demo namespace
has every kind.

A detail or Related section that *failed* (#348; #275 had judged it
unreachable) needs the server to die after a list has loaded, so that the
detail's own watch has no snapshot. Put a TCP forwarder between the app
and the apiserver and kill it at the right moment (no `socat` on this
machine; a ten-line Python `socket` forwarder on a spare port, with the
kind apiserver's port as upstream, passes TLS and client certs through
untouched). Point a kubeconfig copy at it
(`kubectl config view --raw --minify --context kind-astrolabe-dev | sed -E
's#(server: https://[^:]+):[0-9]+#\1:<port>#'`), start the proxy, wait
until `KUBECONFIG=<copy> kubectl get ns` answers (the first call races the
listener), then launch with that kubeconfig:

- Related: open the Deployments list, `kill` the proxy, open `web`. The
  Pods watch starts fresh and fails: the Pods section reads `Cannot reach
  the cluster: ...` in red. The Deployment itself still shows, from the
  list's snapshot.
- Detail: restart the proxy, open Events, `kill` it again, open a row whose
  object's kind no list has watched yet (a Job, an HPA; not a Pod, whose
  watch the Related check already started). The whole tab reads the same
  text in red.

The workspace banner appears when the proxy dies and goes when it is back,
moving the sidebar 45pt: Events was at y 804 with the banner and 760
without, and the first click at the old position hit nothing. Kill the
proxy and the app when done (#348 retro).

Another worktree's check may kill your app (`dev/shot.sh` then says
`found 0`): relaunch from `target/debug/astrolabe` with the same
environment, and do not kill theirs (#348 retro).

Run this check after review fixes. Done when every view the final change touches has been
opened and screenshotted on `kind-astrolabe-dev` in every state the
change adds (populated, empty, forbidden), each screenshot has been read
back, each is embedded in the PR, and every out-of-scope problem the
check finds is a `needs-triage` issue linked from the PR body. Repeat the affected views and replace their evidence after
any later UI change, so the PR shows the final UI diff.

## 0. Rebase onto main

A check on a stale base is redone once main's UI changes land (#75,
#111):

```sh
git fetch origin main && git rebase origin/main
git log --oneline HEAD..origin/main    # done when this prints nothing
```

Each Bash call is a fresh shell: `export SCRATCH=...` is gone by the next
call, and `> $SCRATCH/app.log` then fails on `/app.log`. Write the full
scratchpad path in every command (#230).

## 1. Seed the cluster

```sh
dev/demo.sh apply     # prints this worktree's namespace, astrolabe-demo-<worktree>
```

`dev/demo.yaml` holds synthetic objects for every built-in kind;
`dev/demo.sh` seeds them into a namespace of this worktree's own, so other
worktrees' demos stay untouched. Extend `dev/demo.yaml` first whenever it
lacks a kind or a state the check needs, including states a refactor only
has to keep working (keep it labelled synthetic). Ad-hoc `kubectl` objects
leave evidence nobody can reproduce.

## 2. Launch

```sh
# run_in_background + disable_timeout; process name: astrolabe
ASTROLABE_CLUSTER=kind-astrolabe-dev ASTROLABE_NAMESPACE=$(dev/demo.sh apply) cargo run -p astrolabe
```

Debug builds open `ASTROLABE_CLUSTER` in `ASTROLABE_NAMESPACE` at launch
(#111); release builds ignore both. `dev/demo.sh apply` is idempotent and
prints only the namespace on stdout (kubectl's output goes to stderr). A
context that is not in the kubeconfig logs a `warn` and the app opens the
first context in `default` instead, and a failed `dev/demo.sh apply` leaves
`ASTROLABE_NAMESPACE` empty, so check the first screenshot shows
`kind-astrolabe-dev` selected and `astrolabe-demo-<worktree>` as the
namespace.

A debug build takes a while to show its window after `cargo run` prints
`Running`. Take `start` first and read it before the first click: #166's
clicks, sent after a fixed sleep, landed on Namespaces instead of Pods and
the run was redone.

## 3. Look and drive, via Orca computer use

`orca computer` drives the window from the shell (the bundled guide:
`orca skills get computer-use`). GPUI exposes almost no accessibility tree,
so navigate by coordinates read off a screenshot:

```sh
dev/shot.sh "$SCRATCH/shots" start                              # get-app-state
dev/shot.sh "$SCRATCH/shots" pods click --x 263 --y 204         # act, then capture
dev/shot.sh "$SCRATCH/shots" table --crop 240,120,640,300       # x,y,w,h in window points
```

`dev/shot.sh` runs the orca action against the astrolabe started from this
worktree (other worktrees run their own, and the process name would pick
any of them), keeps the Retina original as `<name>-orig.png` and prints the
path of `<name>.png`, shrunk to one pixel per window point.

- Read only the shrunk copy. A Retina original carries four times the
  pixels, and eight of them overflowed #20's context window. At one pixel
  per point, coordinates read off the image are click coordinates as they
  stand.

  Keep the `-orig.png` for the PR's evidence. When only one region
  matters, pass `--crop` with the rectangle read off an earlier shrunk
  copy: a whole window still reads at 120-170KB, a cropped table at about
  16KB (#68).
- A `--crop` capture is offset: a button seen at (x, y) in a cropped
  image is at (x + crop x, y + crop y) in the window. #300 read the Logs
  picker's `app` button off a `--crop 440,120,...` shot and clicked its
  crop coordinates, which hit nothing; the second try added the origin
  (702,195). To fill an input again, Cancel the form and reopen it:
  `press-key BackSpace` left the old text in place (its error output was
  discarded, cause not checked) (#300 retro).
- Resize the window, and bring it to the front, with:

  ```sh
  dev/resize.sh 900 800
  ```

  It finds this worktree's app by pid. Do not tell System Events
  `process "astrolabe"`: with another worktree's app running, that
  resizes theirs, and its window then covers this one (#98). Right after
  launch it fails with `-10006` until the window exists; retry once a
  screenshot shows the window (#99).
- `orca computer scroll` moves no GPUI scroll view. Scroll a view
  vertically (a detail pane, a long list) with real wheel events, then
  capture (#99):

  ```sh
  dev/scroll.sh 860 600 down 15     # window point, up|down, wheel lines
  dev/shot.sh "$SCRATCH/shots" related
  ```

  A horizontal scroll and the terminal's scrollback are unproven this
  way: prove them with a UI test that sends `ScrollEvent`
  (`crates/app/tests/autoscaling_quotas_views.rs`,
  `crates/terminal/tests/view_scroll.rs`) (#68, #73).
- In an Exec terminal, `orca computer type-text` arrives garbled
  (`seq 1 200` types `aaa a aaa`). Send each key with `press-key`
  (`space`, `Return` for the specials) (#73).
  Through `dev/shot.sh` the key goes after `--key`: `dev/shot.sh "$D" k
  press-key --key Return`; `press-key Return` fails with `Unknown command:
  computer press-key Return` (#304 retro).
- Logs and Exec status lines on `kind-astrolabe-dev`, Pods > `multi` (1000x1000
  window), each reachable without waiting: the detail row (480,288), then
  Logs (472,146) or Exec (530,146), then the picker button at y=195. Logs
  `setup` (645) is a finished init container: `Stream ended`; Logs `app`
  (702): `Streaming…`. Exec `app` (638) then `sh` (646): `Session running`;
  `press-key --key` `e x i t Return` then `Exited with code 0`; `bash` (703)
  is not in busybox: the red `Session failed: ... "bash": executable file
  not found`. A second tab needs the detail re-opened: click Namespaces
  (278,138), then Pods (252,204), then the row, because the Pods list tab is
  replaced by the Logs or Exec tab. Crop `440,85,560,300`. The Exec crops read
  `DIFF` against a baseline for terminal content only (an extra prompt line,
  the container id in the failed text): compare the status bar by eye and
  say so in the PR (#304 retro).
  Reopening a live tab (#332): the same clicks again select the open tab.
  `app`'s Logs print nothing, so a restart would look the same; prove
  "not restarted" with Exec: `press-key --key` `l` `s` `Return` after `sh`,
  reopen, pick `sh` again, and the `ls` output is still in the terminal
  (a restart gives a blank one). `--crop` goes before the action:
  `dev/shot.sh "$D" name --crop 440,85,560,300 click --x 702 --y 195`;
  after the action it fails with `Unknown flag --crop for command: computer
  click` (#332 retro).
- Edit YAML on that detail is at (714,146); the Editor tab's header and
  first lines crop as `440,85,560,130`. A move-only ticket whose criterion
  says "opening a Detail, Logs, Exec and Editor tab still works" still runs
  the pixel comparison below: #331 first shipped with a smoke walk and a
  note that no comparison was made, and the user asked for it. The
  comparison, with the clicks above in one `actions.sh`, read `same` on
  Detail, Logs (`setup`, `app`), Exec (`app`, `sh`) and Editor. Pods read
  `DIFF` for the `crashy` Pod's Restarts alone (`2 (19s ago)` against
  `3 (56s)`, it crash-loops on purpose): crop the Pods list to the left of
  Restarts or read that cell as the cluster, not the view (#331 retro).
  #342 (a model-type change behind the same tab opens) repeated that walk
  and read the same way, plus one new `DIFF`: the Editor crop
  (`440,85,560,130`) shows the text caret on one side only, because it
  blinks. Read the pair by eye and call it the caret. No Python imaging
  module is installed, so there is no pixel-diff of the two crops beyond
  `dev/shot-same.sh`'s verdict (#342 retro).
- A tab count cannot be read off a capture: tab titles carry the object's
  namespace (`astrolabe-demo-<worktree>`), so with two or three tabs open the
  tab bar overflows a 1000-wide window and shows only the active title. The
  `...` at its right end offers only Zoom In and Close, not a tab list.
  Prove "one detail tab, two Logs tabs" with the `dock_tabs.rs` window tests
  (`dock-tab-N` selectors), and say in the PR that the screenshots show the
  Logs tab streaming and nothing about the count (#303 retro).
- To fill the YAML editor, put the text on the clipboard and paste it:
  `pbcopy < file.yaml`, then `dev/shot.sh "$D" pasted hotkey --key cmd+v`.
  `press-key` takes no modifier chord; `hotkey` does (#172).
- A Global apply error (the editor's red notice bar) needs a document the
  server answers with something other than 409 or 422: a ConfigMap whose
  `metadata.namespace` does not exist gives a 404, `namespaces "x" not
  found`. Paste two such documents separated by `---` to see one line per
  document (#240).
- `dev/shot.sh "$D" name hotkey --key cmd+shift+Return` is the YAML
  editor's Apply, and `cmd+a` then `cmd+v` replaces the buffer with the
  clipboard (#299).
- A top-level unknown field (`bogus: 1`) in a ConfigMap shows a red
  outline and no status-bar note: the apiserver answers it as a typed
  patch failure, which the editor treats as an error. The "unknown
  field, apply will ignore it" warning needs a strict-decoding cause, so
  leave that one to the `EditLoop` test (#299).
- `press-key` sends no function keys: `F6` fails with `unsupported
  key 'F6'`. Reach the action another way: the YAML editor's Precheck
  also runs on its own about a second after an edit (#160). That
  automatic run writes no notices, so a *manual* Precheck (the notice
  paths) needs F6 itself: `osascript -e 'tell application "System Events"
  to key code 97'` reaches the focused editor, and #314's check used it.
  The command palette's "Run dry-run precheck now" did not confirm under
  `orca` until #317 (a deferred callback used `update_in`, which fails
  without a current window): since then `cmd+shift+p`, `Down`, `Return`
  and a click on the row each close the palette. The auto-run Precheck
  masks the result on screen, so the closed palette is the evidence and
  the `yaml_editor` UI tests count the Prechecks (#317).
- An edit made *while* an Apply is in flight (the line marks re-mapped
  onto the newer buffer) cannot be driven by hand: on `kind-astrolabe-dev`
  the 422 comes back before `orca computer` can send a second key. Drive
  the plain 422 line mark on kind, cover the re-map with a kube unit test
  (`ApplyReport::remap`), and say in the PR that the in-flight path was not
  driven by hand (#241 retro).
- A YAML editor conflict (409) needs no extra setup. `dev/demo.sh` seeds
  with `kubectl apply`, so `kubectl-client-side-apply` owns every demo
  field. Change one in the editor and Apply to get the conflict and
  Force Apply (#160).
- A notice above a table pushes the rows down (the Events list's
  `Cannot open ...` line moved them about 39pt). After a click that raises
  a notice, take a fresh capture before the next click: #216's second
  click landed between rows. A click that raises a notice on an Events
  row needs `dev/demo.yaml`'s synthetic Events (unregistered kind,
  nameless reference) (#216). They list last (no age): in a 1000x1000
  window, `dev/scroll.sh 700 600 down 90` puts the nameless one at y 899
  and the unregistered `gadget/g1` at y 931, and clicking a Reason cell
  opens a row. The first rows are the cluster's live events, whose order
  changes whenever the namespace is deleted and re-seeded: a "first row"
  click opened an HPA in one run and a Pod in the next, so a before/after
  of an Events click needs one seeding for both runs, and the notices are
  the comparable shots (#302 retro).
- Texts a change moves between layers (#347: Scale, delete cascade, Events
  row-open and YAML apply notices, the session banner) run as one
  `actions.sh <binary> <outdir>` per binary, each launch fresh, with
  `sleep 10` before `dev/resize.sh 1000 1000`. Coordinates at 1000x1000:
  Pods (252,204), row (480,288), Delete (794,146) shows the cascade;
  Deployments (278,237), row (480,170), Scale (475,147), then
  `type-text --text x` and Apply (718,195) shows `Replicas must be a whole
  number...` beside Delete (`press-key --key Return` does not submit the
  form); Events (258,760), `dev/scroll.sh 700 600 down 90`, the nameless
  event (700,899). Run the baseline binary from inside this worktree:
  `dev/shot.sh` finds the app by its working directory, so a baseline built
  in `$SCRATCH/base` still launches from the repo root. Ages read `DIFF`
  (`2m42s` against `3m34s`), cascade, Scale notice, YAML error and banner
  read `same`. A dead-port kubeconfig (above) with no
  `ASTROLABE_NAMESPACE` shows the session banner on the launch view. In
  this build a two-document paste (`---`) gave one `document 1: missing or
  non-string metadata.name` line, so a multi-line `global-notice` was not
  reachable by hand; say so in the PR (#347 retro).
- Texts a change moves out of a detail view and the namespace menu (#345:
  condition lines, replica summary, tab title, namespace names) reuse that
  script. At 1000x1000: Deployments (278,237), row (480,170); the Delete
  button is at (754,146) there (Pods' is (794,146)) and its question shows
  in crop `440,85,560,140`; the Status section with the replica summary
  and both conditions is crop `440,570,560,250`; the namespace selector is
  (480,57) and its open menu crop `228,40,520,420`. That last crop reaches
  the detail's `Created ... ago` under the menu, so it read `DIFF` for the
  age alone while the menu was identical by eye: crop `228,40,520,320` to
  stop above it. Tab bar, Status and Delete read `same` (#345 retro).
- The packaged `.app` (#371): run `dev/package.sh`, then start
  `target/package/Astrolabe.app/Contents/MacOS/astrolabe` from this
  worktree in a background shell. The binary inside the bundle is the
  packaged artifact, and `dev/shot.sh` finds it by its working directory,
  which `open` would not set. A release build ignores `ASTROLABE_CLUSTER`
  and `ASTROLABE_NAMESPACE`, so it opens the first kubeconfig context in
  `default`: the check is that the sidebar lists every context and Pods
  loads. It is not a Finder launch, which has no shell `KUBECONFIG` or
  PATH; say so in the PR (#371 retro).
- A baseline app that was explored by hand keeps its tabs (a detail opened
  by a probe click): its tab bar then differs from the fresh `after`
  launch. Relaunch the baseline before the scripted run, not only the
  exploration's dropdowns (#302 retro).
  #343 (the Scale input derived from the model) hit it again: the baseline
  was explored to find coordinates, the script ran on it, and all ten
  pairs read `DIFF` for the tab bar alone. Explore on the branch build,
  and launch the baseline fresh only for the scripted run (#343 retro).
- A crop must hold the control under test: #343's first `--crop` was 100
  tall and cut the Scale form (y 195) off, so the pairs said nothing
  about it. Open the form once at full window size, then pick the crop
  (`440,85,560,140` holds the toolbar and the Scale form). When a change
  moves a focus call from a click handler into `render`, the capture
  shows it: the caret is in the input and `type-text` lands without a
  click (#343 retro).
- The Pods crop `440,85,470,320` still holds the Restarts column, so the
  `crashy` Pod reads `DIFF` (`3` against `4`) on every refactor check.
  #346 saw it again and read the pair by eye. Crop Pods to `440,85,230,320`
  (Name to Status) when Restarts is not under test (#346 retro).
- To hover (tooltips), click the element and read the screenshot about
  two seconds later; the pointer stays where it clicked.
- A view that changes with time (ages): `dev/demo.yaml` objects keep
  their first creation time, so delete and re-apply the object for a
  young age. Record its `resourceVersion`
  (`kubectl get … -o jsonpath='{.metadata.resourceVersion}'`) before and
  after the screenshots to show no watch event moved the view (#70).
- The app has only the light theme: nothing calls
  `Theme::sync_system_appearance`. Where an issue asks for a dark-theme
  check, state that it was not checked and why.
- Every action already captures: pass it to `dev/shot.sh` and read the
  result rather than taking another snapshot.
- A refused action fails `dev/shot.sh` with orca's message and writes no
  capture; never pipe its stdout to `/dev/null`, or the previous capture
  passes for this one. `window_not_focused` with "current: no focused
  window", which `--restore-window` does not fix, means a macOS permission
  dialog holds the focus; `dev/shot.sh` then prints each dialog's text.
  These are security decisions: ask the user to answer them, or which
  answer to click (#96).
- A notice that only lasts while a request is in flight ("Scaling…") is
  replaced by the result before a capture lands on kind. Capture the
  result, and leave the in-flight text to a unit test (#96).
- The launch's env vars already select `kind-astrolabe-dev` and this
  worktree's namespace: no clicks before the first screenshot. When a check
  must switch namespace by hand, capture the open selector before picking:
  it lists namespaces alphabetically, and a click at a remembered position
  lands on another namespace once the list changes (#95).

A change to what the launch opens (#344: which Cluster and namespace,
which tabs) is a "must leave the UI as it was" check on three launches,
each with a fresh app, the baseline's and then the branch's: a listed
`ASTROLABE_CLUSTER` with `ASTROLABE_OPEN=Deployment,Secret/db` (a list
tab and a detail tab), an unlisted `ASTROLABE_CLUSTER=nope` (falls back
to the first cluster) and no preselection. Put launch, `dev/resize.sh
1000 1000`, one `dev/shot.sh` and `kill "$(dev/shot.sh --pid)"` of each
into one script run for both binaries, with `sleep 25` after the launch:
the window needs it. The unlisted and none launches read `same`; the
listed one reads `DIFF` for the detail's `Created ... (81s ago)` alone,
the clock (#344 retro).

Another worktree's retro or cleanup runs `pkill astrolabe`, which kills
your app between two Bash calls (#341's first two runs died that way, and
`dev/shot.sh --pid` then says `found 0`). Launch with
`(nohup target/debug/astrolabe > "$S/app.log" 2>&1 &)` after a separate
`cargo build`, not a backgrounded `cargo run &` (it died with its Bash
call), and put launch, resize and every click and capture of one check in
a single call. "A closed Logs tab stops its stream" has no screenshot
form: count the app's connections with
`lsof -nP -a -p "$(dev/shot.sh --pid)" -iTCP -sTCP:ESTABLISHED | tail -n +2 | wc -l`
before opening Logs `app` (3), with it open (5) and after `cmd+w` (4);
without `-a` lsof reads `-iTCP` as a pid. Say in the PR the captures show
the tab and the detail, not the count (#341 retro).

A test-only change (#305: only `crates/app/tests` and docs) has no
production diff to compare against `origin/main`: the launch view and one
list (Pods) on `kind-astrolabe-dev` are the check, and the PR says the
screenshots prove only that. The first `cargo run` took about 90 s to
open its window. `gh pr create --attach` appends the images to the body:
a `![..](./start.png)` placeholder of your own in the body shows a broken
image beside them, so leave the images out of the body text (#305 retro).

### A change that must leave the UI as it was

A refactor proves "unchanged" by comparing pixels against `origin/main`,
not by reading pairs of screenshots: #99 read 36 whole windows and ran out
of context.

1. Build the baseline in a scratch worktree with its own target dir, so
   neither build invalidates the other:

   ```sh
   git worktree add --detach "$SCRATCH/base" origin/main
   (cd "$SCRATCH/base" && CARGO_TARGET_DIR="$SCRATCH/base-target" cargo build -p astrolabe)
   ```
2. Kill this worktree's astrolabe, never by name: `pkill -f
   target/debug/astrolabe` matches every worktree's app and `cargo run`,
   and #342's cleanup killed another worktree's run that way. Use
   `kill "$(dev/shot.sh --pid)"` (#342 retro).
   Kill every running astrolabe of this worktree first, the exploration instance included:
   a probe app left with a dropdown open made #164's first baseline run
   click into it, and every pair read `DIFF`. Then launch it from this
   worktree's directory
   (`"$SCRATCH/base-target/debug/astrolabe"`, run_in_background), so
   `dev/shot.sh` finds it. Run one app at a time: with two, `dev/shot.sh`
   refuses.
3. For each view the change touches, capture with `--crop` a region
   that holds no ages: the header row and column boundaries. Once the
   baseline's clicks and crops are found, write them to a script that
   takes the output directory, and run it for both captures:

   ```sh
   # $SCRATCH/actions.sh
   set -euo pipefail
   D=$1
   dev/shot.sh "$D" start >/dev/null   # a click before any capture can fail
   dev/shot.sh "$D" pods click --x 263 --y 204 >/dev/null; sleep 1
   dev/shot.sh "$D" pods --crop 240,120,640,300 >/dev/null
   ```

   Run `bash "$SCRATCH/actions.sh" "$SCRATCH/before"`, kill the baseline,
   launch this branch, run `bash "$SCRATCH/actions.sh" "$SCRATCH/after"`,
   then:

   ```sh
   dev/shot-same.sh "$SCRATCH/before" "$SCRATCH/after" pods hpa related
   ```

   The script is what makes the runs the same: #97's context was
   compacted between the two runs, the clicks were gone, and only the
   session transcript had them.

   `--crop` goes right after the name, before the action: `dev/shot.sh
   "$D" pods --crop 240,120,640,300 click --x 263 --y 204`. After the
   action it fails with `Unknown flag --crop` (#167).

   A detail pane's top shows `Created ... (2m14s ago)`: a crop holding
   it reads `DIFF` for the age alone. Scroll to the section under test
   (`dev/scroll.sh 860 600 up 80`, then `down 14` reaches a Deployment's
   Related Pods) and crop there (#167).

   Open the same tabs in both runs: an extra tab moves the table down
   10pt and every crop reads `DIFF`.

   Start the script with a click on a sidebar entry. The pointer stays
   where the previous run left it, and a row it hovers is highlighted:
   #148's first comparison read `DIFF` on Namespaces for that alone.
   Seed once and run both builds without `dev/demo.sh delete` between
   them: a re-applied namespace gets new Pod names, and a Pods crop
   reads `DIFF` for that alone. On the dev window, `dev/shot.sh` clicks at
   Namespaces (278,138), Pods (252,204), Definitions (270,825) and the
   first custom kind (280,913); crops that skip the Age columns are
   `440,85,900,320` (Pods) and `440,85,900,360` (Definitions). Those
   coordinates need a window of `dev/resize.sh 1000 1000` (capped at the
   screen, about 950 tall), not the 900x800 above: at 800 tall the
   Definitions click lands outside the window and `dev/shot.sh` fails with
   `cannot identify the background click target`. In a 1000-wide window the
   Pods crop reaches the Age column (`3m18s` against `4m7s` read `DIFF`):
   crop Pods to `440,85,470,320` instead (#244). #223's
   refactor of Session's queries read `same` on five views this way
   (#223). #278 (query shape, built-in scope, error kinds) read `same` on
   Namespaces, Pods, Deployments and Definitions with the click and crop
   coordinates in this paragraph, first try; launch each app as
   `(nohup <binary> > log 2>&1 &)` from the worktree, so it outlives the
   Bash call and `dev/shot.sh` finds it (#278 retro).

   A freshly seeded Deployments or Events list reads `DIFF` for its
   ages alone: Deployments `67s` against `99s`, and Events' `Count` and
   `Last Seen` move with live cluster events. Crop those two to the
   columns left of Age (`440,85,340,320` for Deployments, `440,85,400,320`
   for Events) and read the ages' difference as the clock, not the view
   (#237). The only demo custom kind with an object is Widget `gizmo`:
   scroll the sidebar (`dev/scroll.sh 330 700 down 30`), click Widget
   (272,816), then its row (500,160) to check a custom detail (#237).
4. Read one pair per view, for the PR's Evidence.

Clean up with `git worktree remove "$SCRATCH/base"`.

## 4. Attach to the PR

Copy the screenshots out of the temp directory under descriptive names,
reference each in the PR body's Evidence as `![alt](./shot.png)`, and pass
each to `--attach`; gh uploads them and rewrites the paths:

```sh
gh pr create --body-file body.md --attach ./shot.png   # also gh pr edit / gh pr comment
```

Run it from the directory that holds the shots, so the path passed to
`--attach` is the one the body names. #164 passed absolute scratch paths
while the body said `./shot.png`; the body kept the broken relative link
and gained a second image line (cause unverified). #249 did the same: its
shots sat in the scratchpad, so the body kept `./prechecked.png` and gained
the uploaded images at the bottom. Read the body back with
`gh pr view --json body` and fix it before enabling auto-merge. The
scratchpad is not a git repository, so pass `-R jasonwun/Astrolabe --head
<branch> --base main` there, or `gh` fails with "not a git repository"
(#282 retro; its body came out right: two uploaded images, no `./` link).
#332 wrote the absolute scratchpad path into the body's `![alt](...)` lines
and passed the same path to `--attach` (the path the body names, as above):
the body came back with the four uploaded images in place and no
duplicates (#332 retro).

#313 hit it a third time: the body had `![alt](./shot.png)` lines, `--attach`
took absolute scratchpad paths, and the body kept the four `./` lines and
gained four uploaded images. The shortest way out: leave the `![...]`
lines out of the body and pass `--attach <abs path>`; gh appends the
uploaded images, each captioned with its file name. Delete any `./` line
with `gh pr edit --body-file` before enabling auto-merge (#313 retro).

From the shots' directory (outside the repo) `gh` finds no repository:
push the branch first, then pass `-R jasonwun/Astrolabe --head <branch>
--base main` (#166).

A port-forward row: probing the local port with `nc -z localhost <port>`,
when the container listens on nothing, makes a `Last connection failed:
...` text appear: a quick way to check the row's layout. `nc -z` can
succeed without the text appearing (the error lands only once the
connection is forwarded); `echo x | nc -w 3 localhost <port>` produced it
every time. `lsof -nP -iTCP:<port> -sTCP:LISTEN` reads the listener's
state without a connection (#166, #213).

`dev/resize.sh 2000 800` is capped at the screen: on a 1512pt screen the
capture is 1512 wide. Say so in the PR rather than call it 2000 (#213).

## 5. Clean up

```sh
kill "$(dev/shot.sh --pid)"         # this worktree's app; the background task then exits 143
dev/demo.sh delete                 # this worktree's namespace only
```
