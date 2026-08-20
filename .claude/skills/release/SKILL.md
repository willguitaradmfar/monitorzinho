---
name: release
description: Cut and publish a new monitorzinho release — bump Cargo.toml's version, verify it matches CI, commit, tag, and push so GitHub Actions builds and publishes the release binary. Use when the user asks to "release", "publish a new version", "bump the version", or "cut a release".
---

Releasing monitorzinho is: bump the version in `Cargo.toml`, commit everything
pending, tag `vX.Y.Z`, push. There's no separate build/upload step to run by
hand — pushing the tag is what triggers `.github/workflows/release.yml`,
which builds the Linux x86_64 binary (via `cargo zigbuild`, glibc 2.17
baseline for portability — see the comments in that workflow for why not a
static musl build) and publishes it as a GitHub Release with
auto-generated notes. Pushing to `main` also triggers `.github/workflows/ci.yml`
(fmt/build/clippy) independently of the release build.

## Steps

1. **Decide the version bump.** Check the project's own history for the
   convention rather than assuming strict semver:
   ```
   git log --oneline --decorate --tags -20
   ```
   In practice: a batch of new user-facing features/panels (e.g. "Add
   fullscreen keyboard shortcuts, process kill, and right-anchored
   sparklines", or a new tabs/process-tree/connections release) bumps
   **minor**; a single smaller addition, tweak, or fix bumps **patch**.
   Every release so far has been exactly one commit and one tag — bundle
   everything pending into that one commit rather than splitting it up.

2. **Edit `Cargo.toml`**: bump the `version` field under `[package]`.

3. **Sync `Cargo.lock`** — it embeds the package's own version too, and CI
   builds with `--locked`, so a stale lockfile fails CI:
   ```
   cargo build
   ```
   (a plain build, *not* `--locked`, so it's allowed to rewrite the lockfile)

4. **Verify it'll pass CI before pushing** — mirror `.github/workflows/ci.yml`
   exactly:
   ```
   cargo fmt --check
   cargo build --locked
   cargo clippy --locked --all-targets -- -D warnings
   ```
   Fix anything that fails here; don't push and rely on CI to catch it.

5. **Review what's about to ship**:
   ```
   git status
   git diff --stat
   ```
   Untracked new files (new monitor modules, etc.) need `git add` explicitly
   — there's no blanket `git add -A` here, to avoid catching stray files.

6. **Commit.** Follow the existing log's style: a short imperative summary
   line, a blank line, then a paragraph of *why* (not a changelog of every
   file touched) — read `git log -5` first to match tone. End with:
   ```
   Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
   ```
   (every commit in this repo carries that trailer already — it's not
   optional here, this project is Claude-Code-maintained).

7. **Tag and push** — the tag name must match `v*.*.*` exactly, that's what
   `release.yml` triggers on:
   ```
   git tag vX.Y.Z
   git push origin main
   git push origin vX.Y.Z
   ```

8. **Confirm the workflows actually started** (optional but worth doing —
   catches a broken build immediately instead of the user finding out
   later). If `gh` is available: `gh run list --limit 4`. Otherwise the
   public Actions API works without auth for this public repo:
   ```
   curl -s "https://api.github.com/repos/willguitaradmfar/monitorzinho/actions/runs?per_page=4" \
     | python3 -c "
   import json,sys
   for r in json.load(sys.stdin)['workflow_runs']:
       print(r['name'], '|', r['head_branch'], '|', r['status'], '|', r['conclusion'], '|', r['html_url'])
   "
   ```
   Expect to see both `Release` (tag `vX.Y.Z`) and `CI` (branch `main`) as
   `in_progress` (or already `completed`/`success` if you check a bit later).

## Notes

- Never hand-edit a changelog or write release notes — `generate_release_notes: true`
  in the release workflow builds them from the commit(s)/PRs since the last tag.
- The release binary is Linux x86_64 only (see `README.md`'s Install section)
  — that's a project constraint, not something to "fix" as part of a release.
- If CI or the release build fails after pushing, fix forward with a new
  commit/tag rather than force-pushing or deleting the tag — same rule as
  the general git safety guidance (avoid destructive history rewrites).
