# Testing and releasing

## Which test catches what

Most of the code can be tested without a VM, because the guest's request handling runs on any Unix host and the
fake executor serves it over the same protocol. Real VMs are the last check, not the first.

| Level | Where | Needs | Catches |
| --- | --- | --- | --- |
| Unit tests | `src/**` in every crate (`tests.rs` or `mod tests`) | nothing | the rules in each doc comment: parsing, merge rules, state transitions, locks, wire encoding, Docker behavior |
| Guest handling on the host | [`crates/app/guest`](../../crates/app/guest) unit tests | Unix | exec, copy, egress proxy and DNS over plain sockets |
| Privileged guest tests | `cargo test -p sbx-guest --lib privileged` with `SANDBOX_PRIVILEGED_TESTS=1` | root, loop devices (CI runs them in a privileged container) | the kernel mounting scratch and using it as the overlay upper layer |
| Filesystem checkers | `SANDBOX_REQUIRE_FSCK_EROFS=1`, `SANDBOX_REQUIRE_E2FSCK=1` | `erofs-utils`, `e2fsprogs` | the EROFS and ext2 images we write, validated by the official `fsck` tools |
| Runtime with the in-process fake | [`crates/core/runtime/src/tests.rs`](../../crates/core/runtime/src/tests.rs) | Unix | lifecycle, ownership, names, records, egress, `cp` targets |
| Executor protocol conformance | [`crates/executor/fake/tests/conformance.rs`](../../crates/executor/fake/tests/conformance.rs) | Unix | `sbx-executor-process` against a protocol-speaking binary, including misbehaving ones |
| Firecracker adapter sessions | [`crates/executor/firecracker/tests/session.rs`](../../crates/executor/firecracker/tests/session.rs) | Unix | the protocol session and Firecracker config, with a shell script standing in for `firecracker` |
| CLI end to end | [`crates/app/cli/tests/e2e.rs`](../../crates/app/cli/tests/e2e.rs) | nothing (Windows skips the parts that need Unix sockets) | the real `sbx` binary: flags, output, exit codes, `--rm`, names, `ps`, `logs`, `cp`, MCP |
| Dependency rules | `cargo xtask check-deps` | nothing | layers, per-crate allow lists, forbidden features, module cycles, the map in `crates.md` |
| Real VMs | `cargo xtask smoke` | an installed executor | 17 checks: output and exit codes, stdin, no network device, fast DNS failure, writable root, detached `exec`/`cp`/`logs`/`stop`/`rm`, allow list |
| Numbers | `cargo xtask bench [--docker]` | an installed executor (and Docker for `--docker`) | regressions on the hot path: `run`, `exec`, parallel runs |

When you fix a bug, add the test at the lowest level that can see it. If it took a real VM to find, ask whether a
unit test or the fake executor could have found it.

### Running against real VMs locally

```sh
cargo xtask dist                         # release sbx with the guest init embedded
sbx executor install firecracker         # once: the released executor (or --from ./bundle)
cargo xtask smoke --sbx target/release/sbx
```

`smoke` and `bench` use the same directories and default executor as `sbx` (`SBX_DATA_DIR`, `SBX_CACHE_DIR`, …).
Point them at scratch directories to keep test sandboxes away from your own. `--no-internet` skips the check that
needs an HTTPS site.

## CI

[`ci.yml`](../../.github/workflows/ci.yml) runs on every push to `main` and `feat/**`, and on pull requests.

| Job | Runs on | Does |
| --- | --- | --- |
| `test` | Linux x86_64 and arm64, macOS, Windows | `fmt --check`, `clippy -D warnings`, `test --no-fail-fast`, `doc -D warnings`, `check-deps`. On Linux also `shellcheck install.sh`, both guest initramfs builds, the e2e tests again with the init embedded, and `cargo xtask dist` |
| `privileged` | Linux, in a `--privileged` container | the guest tests that need root and loop devices |
| `vm` | Linux x86_64 with KVM (arm64 runs when the runner has KVM) | builds `sbx`, the adapter and the kernel (cached by its config). It publishes a bundle with upstream Firecracker to a `file://` repository, installs it from there and installs `sbx` with `install.sh`, then runs `smoke` and `bench` (the numbers go to the job summary) |

GitHub's macOS runners can't start guests, so the macOS real-VM path (Hypervisor.framework through
[toollyst/firecracker](https://github.com/toollyst/firecracker)) is checked on a Mac with `cargo xtask smoke`,
before a release.

## The default guest kernel

`cargo xtask kernel --arch x86_64|aarch64` builds Linux 6.1 inside Docker
([`kernel.rs`](../../xtask/src/kernel.rs)). It starts from Firecracker's CI configs and adds the fragment
[`xtask/kernel/sbx.config`](../../xtask/kernel/sbx.config), which turns on what the
[executor protocol](../../crates/spec/executor-proto/src/lib.rs) promises the guest: erofs, overlay, vsock, devpts and
so on. The result is `target/kernel/<arch>/kernel`. CI caches it by the hash of those files.

## Releasing

A release is a tag. Pushing `v<version>` runs [`release.yml`](../../.github/workflows/release.yml):

1. **`version`** fails unless the tag matches the workspace version in `Cargo.toml`.
2. **`sbx`** builds `sbx-<os>-<arch>.tar.gz` (+ `.sha256`) for Linux x86_64 and aarch64 (static musl) and macOS
   arm64, each with the guest init embedded.
3. **`kernel`** builds the default kernel for both architectures.
4. **`executor-linux`** bundles the adapter, upstream Firecracker (checked by its sha256) and the kernel. It installs
   the bundle into the freshly built `sbx` and runs `smoke` on KVM before anything is published. The arm64 bundle is
   published without that run while GitHub's arm runners have no KVM.
5. **`executor-darwin`** runs only when the repository variable `FC_FORK_TAG` is set. It downloads that
   [toollyst/firecracker](https://github.com/toollyst/firecracker/releases) release, checks its sha256, signature and
   `--version` on macOS 15, and bundles it with the adapter and the arm64 kernel.
6. **`publish`** creates the GitHub release with the `sbx` archives, adds every bundle to the official executor
   repository on the `gh-pages` branch (`cargo xtask repo-add --url <release URL>`), uploads the bundles to the
   release, and copies `install.sh` next to the index. GitHub Pages serves that branch at
   <https://toollyst.github.io/sandbox/>.

To cut a release:

1. Bump `version` under `[workspace.package]` in `Cargo.toml` (this updates `Cargo.lock` on the next build) and
   merge it to `main` with CI green.
2. On a Mac, run `cargo xtask dist && cargo xtask smoke` with the macOS executor you are about to ship.
3. Tag and push: `git tag -a v0.2.0 -m "sbx 0.2.0" && git push origin v0.2.0`.
4. Check the result the way a user would: `curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh` into a
   scratch `SBX_INSTALL_DIR`, then `sbx info` and `cargo xtask smoke --sbx <that sbx>`.

A tag whose workflow fails publishes nothing, because `publish` only runs when every build succeeded. Fix the cause,
bump the version and tag again rather than moving a pushed tag.

### Executor bundles and repositories

A bundle is a directory with `sbx-executor` at the top and whatever it needs next to it. For Firecracker that is
`firecracker` and `kernel`. `cargo xtask repo-add --repo <dir> --bundle <dir> --name <n> --version <v>` packs it
and adds it to `<dir>/index.json`. It runs the bundle's `sbx-executor capabilities` for the protocol version and
architecture (pass `--capabilities <file>` with saved output for a bundle that can't run on this host). The OS is
this host's, or `--os linux|darwin`. Upload the directory anywhere static, and users add it with
`sbx executor repo add <name> <url>`. The format is specified in
[`sbx-executor-repo`](../../crates/spec/executor-repo/src/lib.rs).
