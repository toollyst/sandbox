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
| Event log | [`crates/port/events/src/tests.rs`](../../crates/port/events/src/tests.rs) | nothing | concurrent appends, rotation, following across generations, half-written and broken lines |
| CLI output contract | `cargo test -p sbx-cli --lib output` | nothing | that [cli-output.md](cli-output.md) and its JSON Schema match the types in `output.rs` (`SBX_BLESS=1` rewrites them), `--jq` |
| TUI without a terminal | [`tui/model/tests.rs`](../../crates/app/cli/src/tui/model/tests.rs), [`tui/view/tests.rs`](../../crates/app/cli/src/tui/view/tests.rs) | nothing | every key's `sbx` command line, filtering, virtual scrolling, drawing (ratatui `TestBackend`) |
| CLI end to end | [`crates/app/cli/tests/e2e.rs`](../../crates/app/cli/tests/e2e.rs) | nothing (Windows skips the parts that need Unix sockets) | the real `sbx` binary: flags, output, exit codes, `--rm`, names, `ps`, `logs` (timestamps, `--since`), `cp`, `events`, `--filter`, `--jq`, every JSON output read back strictly as its type, completion candidates, executor versions and pins, MCP |
| Dependency rules | `cargo xtask check-deps` | nothing | layers, per-crate allow lists, forbidden features, module cycles, the map in `crates.md` |
| Real VMs | `cargo xtask smoke` | an installed executor | 17 checks: output and exit codes, stdin, no network device, fast DNS failure, writable root, detached `exec`/`cp`/`logs`/`stop`/`rm`, allow list |
| Numbers | `cargo xtask bench [--docker]` | an installed executor (and Docker for `--docker`) | regressions on the hot path: `run`, `exec`, parallel runs |

When you fix a bug, add the test at the lowest level that can see it. If it took a real VM to find, ask whether a
unit test or the fake executor could have found it.

### Running against real VMs locally

```sh
cargo xtask dist                         # release sbx with the guest init embedded
sbx executor install wick                # once: the published VMM and its adapter (or --from, below)
cargo xtask smoke --sbx target/release/sbx
```

`smoke` and `bench` use the same directories and default executor as `sbx` (`SBX_DATA_DIR`, `SBX_CACHE_DIR`, …).
Point them at scratch directories to keep test sandboxes away from your own. `--no-internet` skips the check that
needs an HTTPS site.

## CI

[`ci.yml`](../../.github/workflows/ci.yml) runs on every push to `main` and `feat/**`, and on pull requests.

| Job | Runs on | Does |
| --- | --- | --- |
| `test` | Linux x86_64 and arm64, macOS, Windows | `fmt --check`, `clippy -D warnings`, `test --no-fail-fast`, `doc -D warnings`, `check-deps`. On Linux also `shellcheck` on `install.sh` and `.github/scripts/*.sh`, both guest initramfs builds, the e2e tests again with the init embedded, and `cargo xtask dist` |
| `privileged` | Linux, in a `--privileged` container | the guest tests that need root and loop devices |
| `vm` | Linux x86_64 with KVM (arm64 runs when the runner has KVM) | builds `sbx`, the adapter and the kernel (cached by its config), and makes a VMM package from upstream Firecracker (checked by its sha256). It installs `sbx` with `install.sh`, puts the adapter and the VMM package (as `firecracker@1.17.0`) in a local schema 2 `file://` repository, and installs from there the way a user does (`.github/scripts/smoke-local.sh`). It uses neither wick nor the official repository. Then it runs `smoke` and `bench` (the numbers go to the job summary) |

GitHub's macOS runners can't start guests, so the macOS real-VM path (Hypervisor.framework through
[wick](https://github.com/toollyst/wick)) is checked on a Mac with `cargo xtask smoke`,
before a release.

## The default guest kernel

`cargo xtask kernel --arch x86_64|aarch64` builds Linux 6.1 inside Docker
([`kernel.rs`](../../xtask/src/kernel.rs)). It starts from Firecracker's CI configs and adds the fragment
[`xtask/kernel/sbx.config`](../../xtask/kernel/sbx.config), which turns on what the
[executor protocol](../../crates/spec/executor-proto/src/lib.rs) promises the guest: erofs, overlay, vsock, devpts and
so on. The result is `target/kernel/<arch>/kernel`. CI caches it by the hash of those files.

## Releasing

`sbx` and its executors are released separately. A tag releases `sbx`; the executors (the `wick` VMM packages and
the `firecracker` adapter) are published by hand with `executors.yml`, usually after a new wick release.

### `sbx`

Pushing `v<version>` runs [`release.yml`](../../.github/workflows/release.yml):

1. **`version`** fails unless the tag matches the workspace version in `Cargo.toml`.
2. **`sbx`** builds `sbx-<os>-<arch>.tar.gz` (+ `.sha256`) for Linux x86_64 and aarch64 (static musl) and macOS
   arm64, each with the guest init embedded.
3. **`smoke`** runs on Linux x86_64 with KVM, the same way as the CI `vm` job: it builds the adapter from this
   commit, makes a VMM package from upstream Firecracker, puts both in a local `file://` repository, and has the new
   `sbx` install `firecracker@<version>` from there before `cargo xtask smoke`. A `sbx` release depends on neither a
   wick release nor what the official repository holds, so `sbx` and the executors can be released in any order. If
   the protocol version changes, run `executors.yml` afterwards so the official repository gets a matching adapter.
4. **`publish`** creates the GitHub release with the `sbx` archives and copies `install.sh` to the top of the
   `gh-pages` branch. GitHub Pages serves that branch at <https://toollyst.github.io/sandbox/>.

To cut a release:

1. Bump `version` under `[workspace.package]` in `Cargo.toml` (this updates `Cargo.lock` on the next build) and
   merge it to `main` with CI green.
2. Optionally, on a Mac with a macOS VMM installed (for example `wick`), run `cargo xtask dist && cargo xtask smoke`.
   CI has no macOS VM run, so this is the only check there; it does not gate the release.
3. Tag and push: `git tag -a v0.2.0 -m "sbx 0.2.0" && git push origin v0.2.0`.
4. Check the result the way a user would: `curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh` into a
   scratch `SBX_INSTALL_DIR`, then `sbx info`, and with an executor installed (`sbx executor install wick` once the official repository has
   it), `cargo xtask smoke --sbx <that sbx>`.

A tag whose workflow fails publishes nothing, because `publish` only runs when every build succeeded. Fix the cause,
bump the version and tag again rather than moving a pushed tag.

### Executors (`executors.yml`)

[`executors.yml`](../../.github/workflows/executors.yml) runs only by hand. Give it a
[toollyst/wick](https://github.com/toollyst/wick/releases) release, or nothing for the latest one:

```sh
gh workflow run executors.yml                          # the latest wick release
gh workflow run executors.yml -f wick=v1.18.0+wick.1   # that release, published as wick@1.18.0+wick.1
```

Versions are wick's release tags without the `v`, registered as wick publishes them: `v1.18.0` becomes
`wick@1.18.0`, and a fix release `v1.18.0+wick.1` becomes `wick@1.18.0+wick.1`. `sbx executor install wick` (or
`wick@latest`) picks the highest, and `wick@1.18.0` the newest patch of 1.18.0. The empty input takes the release
wick marks as latest. The workflow publishes it for every platform the
official repository doesn't have yet, and never rebuilds or overwrites what is already there. For each platform, the
VMM package is a directory with `firecracker`, `LICENSE` and `NOTICE`: that wick release's build for the platform, or,
on Linux when the release has none, upstream Firecracker of the same upstream version (checked by its sha256). macOS
is skipped with a warning when the release has no macOS build (`-f require_darwin=true` makes that an error). The
adapter is `sbx-executor-firecracker` plus the default kernel and `KERNEL-SOURCE.txt` (where its GPL-2.0 source
and config are). An adapter version that is already published is reused
as is; a new one is built from the commit you run it on. On Linux with KVM it installs both from a local repository
and runs `cargo xtask smoke`. Then it adds them to `executors/v2/index.json` on `gh-pages` and uploads the archives to
the GitHub releases `wick-<version>` and `firecracker-adapter-<version>`. The comment at the top of the workflow lists
its inputs.

The schema 1 `executors/index.json`, which `sbx` 0.1.x reads, stays as it is.

### Executor repositories and packages

A repository is a static directory: an index and `.tar.gz` packages. There are three kinds of package, all specified
in [`sbx-executor-repo`](../../crates/spec/executor-repo/src/lib.rs):

| Kind | Contents | Index entry |
| --- | --- | --- |
| Self-contained executor | `sbx-executor` at the top and whatever it needs next to it (for Firecracker: `firecracker` and `kernel`) | `executors`, with `protocol` |
| Adapter | the same shape: `sbx-executor` and its files (the `firecracker` adapter has `kernel`), but no VMM | `adapters`, with `protocol` (schema 2) |
| VMM package | no `sbx-executor`, only the VMM and its files; the named adapter runs it | `executors`, with `adapter` instead of `protocol` (schema 2) |

Schema 2 lives at `v2/index.json`. `sbx` reads it first and falls back to `index.json` only when it doesn't exist, so
one repository can serve old clients from `index.json` and new ones from `v2/index.json`. A schema 2 repository
needs every package in `v2/index.json`; a new `sbx` doesn't see `index.json` once `v2/index.json` exists.

[`cargo xtask repo-add`](../../xtask/src/repo.rs) packs a directory and adds it to the index:

```sh
# self-contained executor (schema 1 index.json unless --schema 2)
cargo xtask repo-add --repo <dir> --bundle <dir> --name <n> --version <v>
# adapter
cargo xtask repo-add --repo <dir> --schema 2 --adapter-package --bundle <dir> --name <n> --version <v>
# VMM package run by adapter <a>
cargo xtask repo-add --repo <dir> --schema 2 --vmm-for <a> --arch amd64|arm64 --bundle <dir> --name <n> --version <v>
```

For executors and adapters it runs the package's `sbx-executor capabilities` for the protocol version and architecture
(pass `--capabilities <file>` with saved output for a package that can't run on this host). A VMM package doesn't
speak the protocol, so it takes `--arch` instead. The OS is this host's, or `--os linux|darwin`. `--url <prefix>`
points the index at archives uploaded elsewhere, such as GitHub Releases. Upload the directory anywhere static, and
users add it with `sbx executor repo add <name> <url>`.

To try packages without a repository, install them directly:

```sh
sbx executor adapter install --from ./adapter firecracker@dev
sbx executor install --from ./vmm --adapter firecracker wick@dev
```
