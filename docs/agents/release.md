# Cutting a release

A release is one manually started workflow, `.github/workflows/release.yml`.
It builds the `.dmg` on a macOS runner, creates the tag `v<version>` and a
GitHub **pre-release** with the `.dmg` attached. The notes are
`.github/release-header.md` (Gatekeeper instructions, permissions
placeholder) followed by GitHub's generated list of merged PRs since the
previous release tag. Write each paragraph and list item of the header on
one line: release notes render every single newline as a line break (#380
found `blocks the first launch.<br>` on the v0.0.1 page).

1. **Bump PR.** Change `version` in `[workspace.package]` of the root
   `Cargo.toml`, run `cargo update -p astrolabe --offline` so `Cargo.lock`
   follows, and merge it like any PR. The workflow never commits.
2. **Check locally (optional).** `dev/package.sh` builds the same
   `target/package/Astrolabe.app` and `Astrolabe-<version>.dmg` the workflow
   does. Open the `.dmg` and drag the app to Applications.
3. **Run the workflow.** Actions > Release > Run workflow, branch `main`. It
   fails and creates nothing when the run is not on `main` or the tag
   `v<version>` already exists.
4. **Check the Release.** It is marked Pre-release, carries the `.dmg`, and
   the notes show the header and the PR list. Download the `.dmg` on another
   Mac and launch the app from Finder, as below, and follow the notes'
   First launch steps. #380 measured them on macOS 27.0.1: the first launch
   shows "Not Opened" (Move to Trash, Done) and right-click > Open shows the
   same dialog; Privacy & Security then offers Open Anyway, which asks to
   confirm, then for Touch ID or the admin password, and launches the app.
   A bundle without its ad-hoc bundle signature is called "damaged" instead,
   with no Open Anyway; `dev/package.sh` fails on such a bundle (#380).

Making a Release stable is a manual edit of it on GitHub. The workflow has no
switch for that.

## Check a downloaded .dmg

How #377 drove the downloaded app the way a user starts it.

- **Launch it from Finder.** Use
  `osascript -e 'tell application "Finder" to open POSIX file "/Applications/Astrolabe.app"'`.
  Leave the bundle's binary alone before that first launch: astrolabe has
  no `--version`, so `Contents/MacOS/astrolabe --version` starts the app
  from the shell, and whether the Finder launch after it still gets the
  first Gatekeeper assessment is unknown. #377 killed it and copied the app
  out of the `.dmg` again.
- **Drive the Gatekeeper dialog by pid.** It belongs to `CoreServicesUIAgent`,
  not Astrolabe: `orca computer get-app-state --app "pid:$(pgrep -x CoreServicesUIAgent)"`.
  For the verdict, read syspolicyd with `/usr/bin/log show --last 5m
  --predicate 'process == "syspolicyd"'`. A bare `log` is zsh's builtin.
- **Quarantine a local build like a download.** Without a browser, write the
  flag on a copy of the `.dmg`, mount it and copy the app out: the copy in
  `/Applications` inherits the flag (#380).

  ```sh
  xattr -w com.apple.quarantine "0083;$(printf %x "$(date +%s)");Safari;$(uuidgen)" dl.dmg
  ```

  In #380 the app copied with `cp -R` ran from an AppTranslocation path
  after Open Anyway, with the dialogs as above; a Finder drag was not tried.
  Back up the `/Applications/Astrolabe.app` already there and restore it
  afterwards.
- **Right-click > Open needs foreground input.** In #380 a background
  `orca computer` click closed Finder's context menu without choosing an
  item, and background Down and Return keys did nothing. Open
  the menu with `click --mouse-button right --restore-window` on the row and
  click Open with `--restore-window`; tell the user first, it takes over the
  pointer. `orca computer scroll` did not move System Settings: scroll to
  Security with `target/dev/scroll` (built by `dev/scroll.sh`) at a screen
  point after `activate` (#380).
- **Open Anyway asks for Touch ID or the admin password.** That is the
  user's decision: ask them to answer it, never type a password (#380).
- **Give it the test cluster in `~/.kube/config`.** A Finder launch gets no
  shell `KUBECONFIG`, has `PATH=/usr/bin:/bin:/usr/sbin:/sbin` and a working
  directory of `/`, so it reads only `~/.kube/config` and finds no exec
  plugin outside those four directories (#379). Back the file up, put the
  test cluster's context in it, and restore the backup afterwards.
- **Screenshot it.** `dev/shot.sh` finds an astrolabe by its working
  directory, which is `/` here. Pass `ASTROLABE_PID=$(pgrep -nx astrolabe)`
  after checking that pid's binary is the one in `/Applications`.
  For system dialogs and full-screen shots, take
  `screencapture -R <x>,<y>,<w>,<h>` of the dialog, or shrink the capture with
  `sips --resampleWidth 1000` before reading it. #377's full Retina captures
  read at 385K-560K characters each.
