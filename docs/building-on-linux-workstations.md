# Building on many-core Linux workstations

Notes for building this workspace on a large machine (written against a
24-thread / 58 GiB Arch Linux host with several checkouts building at once).
Each statement is marked **verified** (observed with cargo 1.99.1, 2026-11-03),
**from the docs**, or **observed once**.

## Symptom: `sccache: Compile terminated by signal 15` and no rustc error

`rustc` was killed from outside. With `RUSTC_WRAPPER=sccache` the wrapper
reports the child's death and hides rustc's output, so it looks like an sccache
fault. It is not: check `journalctl -u earlyoom` (or `dmesg` for the kernel OOM
killer) first. **Observed once:** the `warp` crate's test build reached 12-17 GiB
resident in a single `rustc`, and an OOM daemon terminated it at <10% available
memory. Several such builds, plus `rust-analyzer` instances, add up quickly.

## Where host-specific flags belong

- **Do not export `RUSTFLAGS`.** It *replaces* the `rustflags` in
  `.cargo/config.toml`, silently dropping `-C symbol-mangling-version=v0` and the
  wasm `--cfg=web_sys_unstable_apis`. **Verified:** with `RUSTFLAGS` set, a
  build carried none of the repo's flags.
- Put host flags in `~/.cargo/config.toml` under a *target* table, for example
  `[target.x86_64-unknown-linux-gnu] rustflags = [...]`. **Verified:** these merge
  with the repo's `[target.'cfg(all())']` flags. `[build] rustflags` in the same
  file is ignored whenever a target table matches. **Verified.**
- Profile tables in `~/.cargo/config.toml` override the ones in this
  `Cargo.toml` (**from the docs**; **verified** for `debug`). A host-wide
  `[profile.dev] debug = 0` therefore removes line tables from workspace crates too.
- Do not commit `-C target-cpu=...`. `dev-remote` and
  `script/deploy_remote_server` build binaries that run on other machines, and a
  host-tuned binary can die with an illegal instruction there.

## Toolchain

`rust-toolchain.toml` pins 1.92.0, but only `rustup`'s `cargo` honors it. A
distribution `cargo` ignores the file and builds with whatever it ships (1.99.1
here). **Verified** by comparing `rustc --version` with the pin. That toolchain
also links with `-fuse-ld=lld` by default (**verified** via
`rustc --print link-args`), so a custom linker such as mold is rarely worth a hard
dependency.

## Concurrency and caching

- `build.jobs` limits parallelism *per `cargo` invocation*, not per machine. N
  concurrent `cargo test` runs can still start N times that many `rustc`s.
- Checkouts that share one `build.target-dir` serialize on cargo's build lock and
  invalidate each other's fingerprints. Separate target dirs cost disk
  (about 27 GiB for the shared one here).
- `sccache` needs `CARGO_INCREMENTAL=0`. It does not cache `bin`, `proc-macro`
  or `cdylib` outputs. **Observed once:** a 15.7% Rust hit rate across several
  checkouts of this repo; the cause (path-keyed entries) is a hypothesis, not tested.

## Not measured

The `"*".debug = false` profile line has not been benchmarked on the full
workspace: no before/after peak-RSS, build time, or `target/` size. Its
effect on the 12-17 GiB `warp` build is unknown and may be small, since that
memory is likely dominated by the one large crate rather than its dependencies.
