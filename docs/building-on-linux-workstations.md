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

`rust-toolchain.toml` pins 1.92.0. The file is read by rustup's proxies
(`cargo`, `rustc`, ...), not by Cargo itself, and Cargo picks its compiler
separately: `RUSTC`, then `build.rustc`, then `rustc` on `PATH`. So the compiler
that builds this workspace depends on which `rustc` Cargo finds, not on which
`cargo` you ran. A distribution `cargo` with a distribution `rustc` first on
`PATH` never consults the pin; a distribution `cargo` that finds rustup's `rustc`
proxy would (**from the docs**, not tested: rustup is not installed on the host
these notes were written on).

**Verified** on that host: in a directory containing a `rust-toolchain.toml` that
pins 1.92.0, `cargo rustc -- --version` reported rustc 1.99.1 (the distribution
compiler), i.e. the pin was ignored. Check what your own setup uses the same way,
not with a bare `rustc --version`. Do it in a scratch crate, not at this
workspace root: there it fails ("virtual manifest"), and
`cargo rustc -p <crate> -- --version` would first compile that crate's whole
dependency tree.

```bash
d=$(mktemp -d) && cd "$d" && cargo init -q --vcs none --name tc .
cp /path/to/this/checkout/rust-toolchain.toml .
cargo rustc -- --version     # prints the compiler Cargo really invokes
```

That compiler also links with `-fuse-ld=lld` by default (**verified** via
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

## Recommended GUI dev loop (measured)

**Verified (2026-11-03, this host):** for `cargo check -p warp --bin warp-oss --features gui` with
`sccache`, `CARGO_INCREMENTAL=0`, and the shared mergerfs target dir:
- cold (clean target): ~1m 30s–1m 55s wall time depending on cache state
- warm (unchanged): ~0.6–0.9s wall time

Keep host-specific overrides in `~/.cargo/config.toml` (target dir, `jobs`, profile
tweaks) as documented above; do not commit them.

## Not measured

The `"*".debug = false` profile line has not been benchmarked on the full
workspace in isolation: no before/after peak-RSS, build time, or `target/` size. Its
effect on the 12-17 GiB `warp` build is unknown and may be small, since that
memory is likely dominated by the one large crate rather than its dependencies.
