# Cutting a release

A release is one manually started workflow, `.github/workflows/release.yml`.
It builds the `.dmg` on a macOS runner, creates the tag `v<version>` and a
GitHub **pre-release** with the `.dmg` attached. The notes are
`.github/release-header.md` (Gatekeeper instructions, permissions
placeholder) followed by GitHub's generated list of merged PRs since the
previous release tag.

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
   Mac, open it, and launch the app (right-click > Open, or the `xattr`
   line in the notes).

Making a Release stable is a manual edit of it on GitHub. The workflow has no
switch for that.
