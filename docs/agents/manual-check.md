# Manual UI check

`gpui_kit` tests assert what the element tree contains, not what a person
sees: invisible text (#52) and a tab with no close button (#51) both passed
the suite. Before handing over a change that shows in the app, look at it
on the dev cluster and attach the screenshots to the hand-over.

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
- To fill the YAML editor, put the text on the clipboard and paste it:
  `pbcopy < file.yaml`, then `dev/shot.sh "$D" pasted hotkey --key cmd+v`.
  `press-key` takes no modifier chord; `hotkey` does (#172).
- `press-key` sends no function keys: `F6` fails with `unsupported
  key 'F6'`. Reach the action another way: the YAML editor's Precheck
  also runs on its own about a second after an edit (#160).
- A YAML editor conflict (409) needs no extra setup. `dev/demo.sh` seeds
  with `kubectl apply`, so `kubectl-client-side-apply` owns every demo
  field. Change one in the editor and Apply to get the conflict and
  Force Apply (#160).
- A notice above a table pushes the rows down (the Events list's
  `Cannot open ...` line moved them about 39pt). After a click that raises
  a notice, take a fresh capture before the next click: #216's second
  click landed between rows. A click that raises a notice on an Events
  row needs `dev/demo.yaml`'s synthetic Events (unregistered kind,
  nameless reference) (#216).
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
2. Kill every running astrolabe first, the exploration instance included:
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
   `440,85,900,320` (Pods) and `440,85,900,360` (Definitions). #223's
   refactor of Session's queries read `same` on five views this way
   (#223).
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
and gained a second image line (cause unverified). Read the body back with
`gh pr view --json body` and fix it before enabling auto-merge.

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
