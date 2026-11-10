# Developer guide

This is the guide for working on `sbx` itself. If you only want to use it, the [README](../../README.md) is enough.

| Read | When you want to |
| --- | --- |
| This page | Find your way around the repository and run the everyday commands |
| [crates.md](crates.md) | Know what each folder under `crates/` does, what it may depend on, and what it must not know |
| [how-it-works.md](how-it-works.md) | Follow one `sbx run` from the command line into the VM and back |
| [testing-and-releasing.md](testing-and-releasing.md) | Know which test catches what, what CI runs, and how a release is made |
| [cli-output.md](cli-output.md) | Read or change what `--format json` prints (the contract scripts and tools rely on) |

The rules themselves live in code (doc comments, types, tests and `cargo xtask check-deps`), not in these pages. These
pages explain the shape and point to the code. When the two disagree, the code is right, and the page is a bug. The
dependency map in [crates.md](crates.md) is generated, and CI fails when it is stale. So are
[cli-output.md](cli-output.md) and its JSON Schema: `cargo test` fails when they differ from the output types in
`crates/app/cli/src/output.rs`, and `SBX_BLESS=1 cargo test -p sbx-cli --lib output` rewrites them.

Doc comments and the design notes in [`docs/plan/`](../plan/) are written in Korean. The developer guide, the README and
everything a user sees (CLI help, errors) are in English.

## The repository

```text
crates/              the Rust workspace, one folder per layer (see crates.md)
  model/             shared data types
  spec/              public protocols other programs implement
  port/              interfaces and building blocks the cores rely on
  adapter/           implementations of those interfaces
  core/              sandbox lifecycle (runtime) and images (manage)
  app/               binaries: the sbx CLI, the per-sandbox shim, the guest init
  executor/          executors: the Firecracker adapter and the test fake
xtask/               development tasks: cargo xtask <task>
  kernel/            config fragments for the default guest kernel
docs/
  developers/        this guide (English)
  plan/              design notes (Korean)
  roadmap.md         what is done and what is next (Korean)
assets/demo.svg      the README recording, made by cargo xtask demo
install.sh           the installer served from GitHub Pages
.github/workflows/   ci.yml (every push), release.yml (tags v*), executors.yml (by hand)
.github/scripts/     shell steps the workflows share (VMM packages, kernel notice, smoke with wick)
```

Three programs come out of the workspace, plus one that runs inside the VM:

| Program | Crate | Runs where | Job |
| --- | --- | --- | --- |
| `sbx` | `sbx-cli` | on the host, started by the user or an MCP client | Docker-style CLI and MCP server |
| `sbx-shim` | `sbx-shim` | on the host, one per detached sandbox | owns a detached sandbox's VM after `sbx` exits |
| `sbx-executor-firecracker` | `sbx-executor-firecracker` | on the host, one per VM | the `firecracker` adapter: speaks the executor protocol and drives Firecracker |
| `sbx-guest` | `sbx-guest` | inside the VM as PID 1 | mounts the image and runs commands for the host |

`sbx-guest` is not shipped on its own. `cargo xtask dist` builds it for Linux (static musl), packs it into an
initramfs and embeds that in `sbx`.

## Setup

You need stable Rust (the minimum is in `Cargo.toml`, `rust-version`) and the musl target of your machine's
architecture, because the guest init is always built for Linux:

```sh
rustup target add "$(uname -m | sed s/arm64/aarch64/)-unknown-linux-musl"
```

That is all you need for the unit tests, the end-to-end tests and the dependency checks. They use a fake executor
and never start a VM, so they run on macOS, Linux and Windows.

To run real VMs you also need an executor: install the published one with `sbx executor install wick`, on an
Apple Silicon Mac with macOS 15+ or on Linux with `/dev/kvm`. Building the default kernel (`cargo xtask kernel`) needs
Docker.

## Everyday commands

```sh
cargo test --workspace --all-features      # unit, conformance, CLI end to end (no VM)
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
cargo xtask check-deps                     # layers, crate rules, module cycles, crates.md

cargo xtask dist                           # release sbx + sbx-shim, guest init embedded
cargo xtask smoke                          # 17 checks on real VMs with the installed executor
cargo xtask bench                          # timings (add --docker to compare with docker run)
```

| `cargo xtask …` | What it does | Source |
| --- | --- | --- |
| `check-deps` | Every rule about who may depend on whom, and that `crates.md` is current | [`main.rs`](../../xtask/src/main.rs), [`layout.rs`](../../xtask/src/layout.rs), [`modules.rs`](../../xtask/src/modules.rs) |
| `deps-doc` | Regenerates the dependency map in `crates.md` | [`layout.rs`](../../xtask/src/layout.rs) |
| `dist [--target T]` | Builds the guest initramfs and a release `sbx` that embeds it, plus `sbx-shim` | [`dist.rs`](../../xtask/src/dist.rs) |
| `guest-initramfs --arch A` | Only the initramfs (for a development build that doesn't embed it) | [`initramfs.rs`](../../xtask/src/initramfs.rs) |
| `kernel --arch A` | The default guest kernel, built in Docker | [`kernel.rs`](../../xtask/src/kernel.rs) |
| `smoke [--sbx F]` | Real-VM checks: pull, exec, cp, logs, stop, egress… | [`smoke.rs`](../../xtask/src/smoke.rs) |
| `bench [--docker]` | p50/p90 of `run`, `exec` and parallel runs | [`bench.rs`](../../xtask/src/bench.rs) |
| `demo` | Re-records `assets/demo.svg` from real commands | [`demo.rs`](../../xtask/src/demo.rs) |
| `repo-add` | Adds an executor, adapter or VMM package to a static executor repository | [`repo.rs`](../../xtask/src/repo.rs) |

A plain `cargo build` gives you an `sbx` without the guest init. It works for everything that doesn't boot a VM. For
real VMs, use `cargo xtask dist`, or point a development build at an initramfs with `--guest-init`.

## Making a change

1. **Find the layer.** [crates.md](crates.md) says which crate owns what. If a change needs a lower crate to
   know about a higher one, the design is wrong somewhere, and `check-deps` will say so.
2. **Change the rule where it lives.** Behavior is documented in the doc comment of the module that implements it
   (see [Where the rules live](#where-the-rules-live)). Update that comment in the same change.
3. **Test at the lowest level that can see the bug**: a unit test in the crate, then the fake-executor end-to-end
   tests in `sbx-cli`, then `cargo xtask smoke` if it touches the VM.
4. **Run the four checks** (`test`, `clippy`, `fmt`, `check-deps`). CI runs the same ones on Linux, macOS and Windows,
   and boots real VMs on Linux x86_64.

Two principles shape most review comments:

- **Cores are mechanism, not policy.** `sbx-runtime` and `sbx-manage` do exactly what they're asked, and fail
  clearly when they can't. They never fall back to something else, never pick an implementation, and never clean
  up on their own schedule. Decisions belong to the caller (the CLI, the MCP server, or an embedding app).
- **Docker is the reference for the CLI.** A user should be able to guess flags and exit codes. Every deliberate
  difference is listed in [`behavior.rs`](../../crates/app/cli/src/behavior.rs) (`DOCKER_DIFFERENCES`).

## Where the rules live

| Rule | Source of truth |
| --- | --- |
| Which crate may depend on which | `RULES` and `FORBIDDEN_FEATURES` in [`xtask/src/main.rs`](../../xtask/src/main.rs) |
| Layer folders and their order | `LAYERS` in [`xtask/src/layout.rs`](../../xtask/src/layout.rs) |
| No module cycles inside a crate | [`xtask/src/modules.rs`](../../xtask/src/modules.rs) |
| The executor protocol (what an external executor implements, what the guest can rely on) | [`sbx-executor-proto`](../../crates/spec/executor-proto/src/lib.rs) |
| Host↔guest messages: connection order, encoding, trust boundary | [`sbx-guest-proto`](../../crates/spec/guest-proto/src/lib.rs) |
| Executor repositories (`index.json`, `v2/index.json`, adapters, VMM packages, choosing a version) | [`sbx-executor-repo`](../../crates/spec/executor-repo/src/lib.rs) |
| Installed executors, VMM packages and adapters on disk | [`sbx-executor-install`](../../crates/spec/executor-install/src/lib.rs) |
| Which executor a command uses (`--executor`, sandbox pin, `.sbx-executor`, default) | [`choose.rs`](../../crates/app/cli/src/choose.rs) |
| Sandbox states and transitions | `State::can_transition_to` in [`sbx-model`](../../crates/model/src/lib.rs) |
| How image config and caller values combine (Docker `run`) | [`crates/core/runtime/src/process.rs`](../../crates/core/runtime/src/process.rs) |
| What an executor must support for a given request | [`crates/core/runtime/src/validate.rs`](../../crates/core/runtime/src/validate.rs) |
| Sandbox records in `data_dir` | [`crates/core/runtime/src/records.rs`](../../crates/core/runtime/src/records.rs) |
| Egress allow list | `AllowRule` in `sbx-model`, enforced in [`crates/core/runtime/src/egress.rs`](../../crates/core/runtime/src/egress.rs) |
| Cache layout, locks, in-use protection | [`sbx-store`](../../crates/port/store/src/lib.rs) |
| Layer merge rules (whiteouts, opaque dirs, hard links) | [`sbx-rootfs`](../../crates/port/rootfs/src/lib.rs) |
| Image references and the OCI scope | [`reference.rs`](../../crates/adapter/oci/src/reference.rs), [`scope.rs`](../../crates/adapter/oci/src/scope.rs) |
| CLI commands, behavior and Docker differences | [`commands.rs`](../../crates/app/cli/src/commands.rs), [`behavior.rs`](../../crates/app/cli/src/behavior.rs) |
| What `--format json` prints, `--jq`, JSON errors ([cli-output.md](cli-output.md)) | [`output.rs`](../../crates/app/cli/src/output.rs) |
| `--filter` keys and how they combine | [`filter.rs`](../../crates/app/cli/src/filter.rs) |
| Event log files (`sbx events`): append, rotation, following | [`sbx-events`](../../crates/port/events/src/lib.rs); what is recorded: the `//!` docs of [`sbx-runtime`](../../crates/core/runtime/src/lib.rs) and [`sbx-manage`](../../crates/core/manage/src/lib.rs) |
| `main.log` format (timestamps, old files without them) | [`crates/app/shim/src/log.rs`](../../crates/app/shim/src/log.rs) |
| Shell completion candidates | [`complete.rs`](../../crates/app/cli/src/complete.rs) |
| `sbx tui` keys (the help and `sbx tui --help` come from it) and how it runs `sbx` | [`tui/keys.rs`](../../crates/app/cli/src/tui/keys.rs), [`tui/mod.rs`](../../crates/app/cli/src/tui/mod.rs) |
| Where the `firecracker` adapter finds `firecracker` and the kernel | [`sbx-executor-firecracker`](../../crates/executor/firecracker/src/lib.rs) |
| Default directories | [`dirs.rs`](../../crates/app/cli/src/dirs.rs) |
| Guest boot sequence | [`boot.rs`](../../crates/app/guest/src/boot.rs) |
