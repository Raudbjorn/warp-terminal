# Contributing

Thanks for helping with this fork of Warp. This file covers the workflow in
this repository. For how the fork relates to upstream Warp, see
[UPSTREAM.md](UPSTREAM.md). For local AI behavior, see
[docs/local-ai.md](docs/local-ai.md).

## Reporting problems and ideas

- Bugs, feature requests and questions go in this repository's
  [issues](https://github.com/Raudbjorn/warp-terminal/issues). Search first.
  A good bug report has reproduction steps, what you expected, and what
  happened. Issues are labeled `bug`, `enhancement`, `documentation` or
  `dependencies`.
- **Security vulnerabilities do not go in public issues.** See
  [SECURITY.md](SECURITY.md).

## Build and run

```bash
./script/bootstrap   # one-time toolchain and system dependencies
./script/run         # build and run the OSS channel from source
```

The Rust toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml).
The project builds for Linux only. See the README for release bundles.

## Checks to run before you open a pull request

```bash
./script/presubmit
```

`presubmit` runs, in order:

1. `./script/format --check` (rustfmt, edition 2024; run `./script/format` to fix)
2. `./script/check_no_inline_test_modules`
3. `cargo clippy --workspace --all-targets --tests -- -D warnings`, and the same
   for `-p warp`
4. `clang-format` on `crates/warpui/src` and `app/src`, and `wgslfmt` on `*.wgsl`

This repository has no CI workflows, so these run only when you run them.
`presubmit` does not currently pass on a clean `master`; the failures are
tracked in
[#34](https://github.com/Raudbjorn/warp-terminal/issues/34),
[#35](https://github.com/Raudbjorn/warp-terminal/issues/35) and
[#36](https://github.com/Raudbjorn/warp-terminal/issues/36). Make sure your
change does not add new failures, and say in the pull request which checks you
ran.

Also run the tests of the crates you touched, for example
`cargo test -p node_runtime --features local_fs`.

## Tests

Inline `mod tests { ... }` blocks are not allowed. Put test code in a sibling
`*_tests.rs` file and include it from the module under test:

```rust
#[cfg(test)]
#[path = "foo_tests.rs"]
mod tests;
```

Add a test for the behavior you change. A regression test should fail without
your fix; say so in the pull request if you checked.

## Local-only behavior

The OSS build runs in `ServicesMode::LocalOnly`. Code that makes network
requests must go through `http_client::Client` (which applies
`network_policy`), or build its `reqwest` client with
`network_policy::apply_to_client_builder` and check the target with
`network_policy::check_url`. A bare `reqwest::Client::new()` skips the policy,
the per-hop redirect check and the proxy bypass.

## Dependencies

- Prefer a crate already in the workspace over a new one.
- `deny.toml` and `about.toml` hold the same list of accepted licenses;
  `./script/check_license_config_sync` fails if they diverge.
- Keep `Cargo.lock` and `yarn.lock` changes minimal and explain any pin you add.
  Resolve with `CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback` when a
  newer crate version needs a newer Rust than the pinned toolchain.

## Pull requests

- Branch from `master`.
- Keep a pull request to one topic, and describe what changed, why, and how you
  verified it. State plainly what you did not verify.
- Automated reviewers may comment on your pull request. Treat their comments as
  review: fix the problem, or reply with why it does not apply.
- Link the issue the change addresses (`Fixes #123`).

## License

By contributing you agree that your contribution is licensed as described in
the README: MIT for the `warpui_core` and `warpui` crates, AGPL-3.0 for
everything else.
