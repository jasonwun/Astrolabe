# Manual UI check

`gpui_kit` tests assert what the element tree contains, not what a person
sees: invisible text (#52) and a tab with no close button (#51) both passed
the suite. Before handing over a change that shows in the app, look at it
on the dev cluster and attach the screenshots to the hand-over.

Run this check after review fixes, on a branch rebased onto
`origin/main`: a check on a stale base is redone once main's UI changes
land (#75). Done when every view the final change touches has been
opened and screenshotted on `kind-astrolabe-dev` in every state the
change adds (populated, empty, forbidden), each screenshot has been read
back, each is embedded in the PR, and every out-of-scope problem the
check finds is a `needs-triage` issue linked from the PR body. Repeat the affected views and replace their evidence after
any later UI change, so the PR shows the final UI diff.

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
cargo run -p astrolabe     # run_in_background + disable_timeout; process name: astrolabe
```

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
- Resize the window with System Events:

  ```sh
  osascript -e 'tell application "System Events" to tell process "astrolabe" to set size of window 1 to {900, 800}'
  ```

  Right after launch it fails with `-10006` until the window exists;
  retry once a screenshot shows the window (#99).
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
- Select `kind-astrolabe-dev` in the Clusters sidebar if it is not
  already active, then this worktree's namespace in the namespace selector.
  Capture the open selector before picking: it lists namespaces
  alphabetically, so `astrolabe-demo-*` usually sits first, and a click at
  a remembered position lands on another namespace once the list changes.

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
2. Launch it from this worktree's directory
   (`"$SCRATCH/base-target/debug/astrolabe"`, run_in_background), so
   `dev/shot.sh` finds it. Run one app at a time: with two, `dev/shot.sh`
   refuses.
3. For each view the change touches, capture with `--crop` a region
   that holds no ages: the header row and column boundaries. Capture into
   `before/`, kill the baseline, launch this branch and repeat the same
   actions into `after/`, then:

   ```sh
   dev/shot-same.sh "$SCRATCH/before" "$SCRATCH/after" pods hpa related
   ```

   Open the same tabs in both runs: an extra tab moves the table down
   10pt and every crop reads `DIFF`.
4. Read one pair per view, for the PR's Evidence.

Clean up with `git worktree remove "$SCRATCH/base"`.

## 4. Attach to the PR

Copy the screenshots out of the temp directory under descriptive names,
reference each in the PR body's Evidence as `![alt](./shot.png)`, and pass
each to `--attach`; gh uploads them and rewrites the paths:

```sh
gh pr create --body-file body.md --attach ./shot.png   # also gh pr edit / gh pr comment
```

## 5. Clean up

```sh
kill "$(dev/shot.sh --pid)"         # this worktree's app; the background task then exits 143
dev/demo.sh delete                 # this worktree's namespace only
```
