# Relationship to upstream Warp

This repository is a fork of [warpdotdev/warp](https://github.com/warpdotdev/warp)
(the GitHub parent of this repo). `master` is the working branch: it carries all
fork changes and is **not** a pristine mirror of upstream.

## History of the sync approach

- The repository started from upstream's "Initial public release of Warp." and
  kept upstream's history for a while.
- Commit `aca3a8be` ("Integrate oh-my-warp patch series (48 patches)") folded
  the patches of the former *oh-my-warp* overlay into the tree. The overlay's
  tooling (`omw`, `patches/`, `OMW.md`) assumed a pristine `master` and a
  separate working branch, and none of its patches applied to the tree any more,
  so it was removed. The plugin host sources still carry "oh-my-warp" in their
  module docs; that is history, not a dependency.
- Since then upstream changes arrive as **resync pull requests** that merge
  upstream into a `resync*` branch and squash-merge it into `master`
  ([#24](https://github.com/Raudbjorn/warp-terminal/pull/24),
  [#25](https://github.com/Raudbjorn/warp-terminal/pull/25)).

## Last synced upstream revision

Not recorded. The resync PRs are squash merges, so git history does not keep
the upstream commit they were cut from; the commit message of each squash lists
the upstream changes it contains (`git show a7b16b82` for "Resync 3").

**At the next resync, write the upstream SHA here**:

| Resync | Date | Upstream SHA |
|---|---|---|
| Resync 3 (#25) | 2026-10-31 | not recorded |

## Resync procedure

This is the procedure the resync PRs used. Adjust it if you find a better one,
and keep this file current.

1. Add the upstream remote once: `git remote add upstream https://github.com/warpdotdev/warp`.
2. `git fetch upstream` and note `git rev-parse upstream/master`.
3. Branch from `master` (for example `resync-4`) and **merge** `upstream/master`
   into it. Do not rebase `master` or the resync branch onto upstream.
4. Resolve conflicts keeping the fork's side where it matters:
   - Files deleted on `master` stay deleted (Resync #24 preserved 537 of
     them). The removals so far: Warp-internal infrastructure, specs and agent
     skills (`93bfa5f9`), Windows assets and Warp governance and tooling
     configs (`df173ae7`), macOS packaging tooling, Sentry and the GitHub
     workflows (`a7b16b82`, Resync 3, and #24), RudderStack (`06d418e9`), and
     the changelog feature (`e9830944`).
   - Keep the fork's local-only behavior: `ServicesMode::LocalOnly` in
     `app/src/bin/oss.rs`, the `network_policy` crate, the local AI client and
     agent loop (`app/src/ai/local_openai.rs`, `app/src/local_multi_agent`,
     `crates/local_multi_agent_service`), the plugin host (`omw_plugins`
     feature) and the SSH changes.
5. Expect conflicts in `Cargo.toml` and `Cargo.lock`, `app/Cargo.toml`,
   `crates/http_client/src/lib.rs`, `crates/network_policy/src/lib.rs`,
   `crates/warp_cli/src/lib.rs`, `app/src/server/server_api.rs` and
   `app/src/settings/ai.rs`. Upstream APIs that the fork's code calls change
   often; fix the fallout rather than reverting the fork's code.
6. Build and test (`cargo check -p warp --lib`, the targeted crate tests, and
   `./script/presubmit` as far as it passes on `master`) and put what you ran,
   and what you did not, in the PR description.
7. Squash-merge, then fill in the table above.

## Things that are deliberately different from upstream

- No Warp backend, account or telemetry in the OSS channel (see
  [docs/local-ai.md](docs/local-ai.md)).
- Linux only: Windows and macOS assets and scripts were removed.
- No GitHub Actions workflows. Run `./script/presubmit` locally.
