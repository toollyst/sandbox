# Contributing

Thanks for looking. Bug reports, benchmarks from your machine, executors for other VMMs and fixes are all welcome.

## Before you start

- **Security issues** go through private reporting, not issues. See [SECURITY.md](SECURITY.md).
- For anything bigger than a fix, open an issue first so we can agree on the shape. The crates have firm rules
  about direction (below), and it's cheaper to talk before code.

## Build and test

You need Rust (stable) and the `*-unknown-linux-musl` target of your machine's architecture for the guest init.

```sh
rustup target add "$(uname -m | sed s/arm64/aarch64/)-unknown-linux-musl"
cargo test --workspace --all-features     # unit, protocol conformance, CLI end to end (fake executor, no VM)
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo xtask check-deps                    # dependency direction between and inside crates
```

With a real executor installed (Linux with `/dev/kvm`, or an Apple Silicon Mac):

```sh
cargo xtask dist && cargo xtask smoke     # 17 checks on real VMs
cargo xtask bench                         # numbers to paste into a PR that touches the hot path
```

If you change code that parses what a guest, a registry, an image or an executor repository sends, also fuzz it
(nightly and `cargo install cargo-fuzz`; see [Fuzzing](docs/developers/testing-and-releasing.md#fuzzing)):

```sh
cargo xtask fuzz                          # every target in fuzz/, 60 s each
```

CI runs all of this on Linux (x86_64 and arm64), macOS and Windows, and boots real VMs on Linux x86_64. Fuzzing runs on
Linux.

## How the code is laid out

- **Layer folders, one-way dependencies.** `crates/` is split by layer: `model`, `spec`, `port`, `adapter`,
  `core`, `app` and `executor`. A crate only depends on lower layers, and the same goes for modules inside a
  crate. `cargo xtask check-deps` enforces both. [docs/developers/](docs/developers/) explains each crate and has
  the dependency map (regenerate it with `cargo xtask deps-doc`; CI fails if it is stale).
- **Cores are mechanism, not policy.** They never pick implementations or make decisions for the caller; binaries
  do. If a change makes a core decide something on the caller's behalf, it probably belongs in the CLI.
- **Docker is the reference for CLI behavior.** Where `sbx` differs, the difference is deliberate and listed in
  [`behavior.rs`](crates/app/cli/src/behavior.rs). Add to that list if you introduce one.
- **Executors speak a public protocol** ([`sbx-executor-proto`](crates/spec/executor-proto/src/lib.rs)). A new VMM is a new
  executor, not a change to the cores.

Code comments and design notes in `docs/` are written in Korean. Write yours in English or Korean, either is fine;
user-facing text (CLI help, errors, README) is English.

## Pull requests

- Keep each PR to one change, with tests that fail without it.
- Run the checks above. If you touched the run or exec path, include `cargo xtask bench` before and after.
- Commits: one logical change each, with a message that says why.

By contributing, you agree that your work is licensed under the project's terms (Apache-2.0 OR MIT).
