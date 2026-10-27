# Manual UI check

`gpui_kit` tests assert what the element tree contains, not what a person
sees: invisible text (#52) and a tab with no close button (#51) both passed
the suite. Before handing over a change that shows in the app, look at it
on the dev cluster and attach the screenshots to the hand-over.

Run this check after review fixes. Done when every view the final change
touches has been opened and screenshotted on `kind-astrolabe-dev`, each
screenshot has been read back, each is embedded in the PR, and every
out-of-scope problem the check finds is a `needs-triage` issue linked from
the PR body. Repeat the affected views and replace their evidence after
any later UI change, so the PR shows the final UI diff.

## 1. Seed the cluster

```sh
kubectl --context kind-astrolabe-dev apply -f dev/demo.yaml
```

`dev/demo.yaml` holds synthetic objects for every built-in kind in the
`astrolabe-demo` namespace. Extend it when a change adds a kind or needs a
shape it lacks (keep it labelled synthetic).

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
```

`dev/shot.sh` runs the orca action, keeps the Retina original as
`<name>-orig.png` and prints the path of `<name>.png`, shrunk to one pixel
per window point.

- Read only the shrunk copy. A Retina original carries four times the
  pixels, and eight of them overflowed #20's context window. At one pixel
  per point, coordinates read off the image are click coordinates as they
  stand.

  Keep the `-orig.png` for the PR's evidence. When only one region
  matters, crop to it as well (`sips --cropToHeightWidth`): a whole
  window still reads at 120-170KB, a cropped table at about 16KB (#68).
- Resize the window with System Events:

  ```sh
  osascript -e 'tell application "System Events" to tell process "astrolabe" to set size of window 1 to {900, 800}'
  ```
- `orca computer scroll` does not move a GPUI horizontal scroll, nor
  the terminal's scrollback. Prove scrolling with a UI test that sends
  `ScrollEvent` (`crates/app/tests/autoscaling_quotas_views.rs`,
  `crates/terminal/tests/view_scroll.rs`) (#68, #73).
- In an Exec terminal, `orca computer type-text` arrives garbled
  (`seq 1 200` types `aaa a aaa`). Send each key with `press-key`
  (`space`, `Return` for the specials) (#73).
- To hover (tooltips), click the element and read the screenshot about
  two seconds later; the pointer stays where it clicked.
- The app has only the light theme: nothing calls
  `Theme::sync_system_appearance`. Where an issue asks for a dark-theme
  check, state that it was not checked and why.
- Every action already captures: pass it to `dev/shot.sh` and read the
  result rather than taking another snapshot.
- Select `kind-astrolabe-dev` in the Clusters sidebar if it is not
  already active, then `astrolabe-demo` in the namespace selector.

## 4. Attach to the PR

Copy the screenshots out of the temp directory under descriptive names,
reference each in the PR body's Evidence as `![alt](./shot.png)`, and pass
each to `--attach`; gh uploads them and rewrites the paths:

```sh
gh pr create --body-file body.md --attach ./shot.png   # also gh pr edit / gh pr comment
```

## 5. Clean up

```sh
pkill -x astrolabe                  # the background task then exits 143
kubectl --context kind-astrolabe-dev delete namespace astrolabe-demo
```
