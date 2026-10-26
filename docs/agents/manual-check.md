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
orca computer get-app-state --app astrolabe --json   # .result.screenshot.{path,scale}
orca computer click --app astrolabe --x 263 --y 302 --json
```

- Read a screenshot only after shrinking it to window points. A Retina
  original carries four times the pixels, and eight of them overflowed
  #20's context window. At one pixel per point, coordinates read off the
  image are click coordinates as they stand (`$P` and `$SCALE` from
  `.result.screenshot`):

  ```sh
  W=$(sips -g pixelWidth "$P" | awk '/pixelWidth/ {print $2}')
  sips --resampleWidth $((W / SCALE)) "$P" --out "$SCRATCH/view.png"   # Read this
  ```

  Keep the original for the PR's evidence. When only one region
  matters, crop to it as well (`sips --cropToHeightWidth`): a whole
  window still reads at 120-170KB, a cropped table at about 16KB (#68).
- Resize the window with System Events:

  ```sh
  osascript -e 'tell application "System Events" to tell process "astrolabe" to set size of window 1 to {900, 800}'
  ```
- `orca computer scroll` does not move a GPUI horizontal scroll. Prove
  horizontal scrolling with a UI test that sends `ScrollWheelEvent`
  (`crates/app/tests/autoscaling_quotas_views.rs`) (#68).
- To hover (tooltips), click the element and read the screenshot about
  two seconds later; the pointer stays where it clicked.
- The app has only the light theme: nothing calls
  `Theme::sync_system_appearance`. Where an issue asks for a dark-theme
  check, state that it was not checked and why.
- Every action returns a fresh `.result.screenshot.path`; read it to see
  the result rather than taking another snapshot.
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
