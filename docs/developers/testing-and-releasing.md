# Testing and releasing

## Which test catches what

Most of the code can be tested without a VM, because the guest's request handling runs on any Unix host and the
fake executor serves it over the same protocol. Real VMs are the last check, not the first.

| Level | Where | Needs | Catches |
| --- | --- | --- | --- |
| Unit tests | `src/**` in every crate (`tests.rs` or `mod tests`) | nothing | the rules in each doc comment: parsing, merge rules, state transitions, locks, wire encoding, Docker behavior |
| Guest handling on the host | [`crates/app/guest`](../../crates/app/guest) unit tests | Unix | exec, copy, egress proxy and DNS over plain sockets; turning an overlay upper directory into build-layer entries |
| Privileged guest tests | `cargo test -p sbx-guest --lib privileged` with `SANDBOX_PRIVILEGED_TESTS=1` | root, loop devices (CI runs them in a privileged container) | the kernel mounting scratch and using it as the overlay upper layer; build layers stacked by the real overlayfs (whiteouts, opaque directories, `redirect_dir=off`, a busy layer, the 500-layer limit), exported and replayed with the OCI rules; transparent TCP's netlink `local` route for `198.18.0.0/15` and a `/32`, listening and connecting on those addresses |
| Guest clock on a real kernel | `cargo test -p sbx-guest --lib privileged_kernel_clock` with `SANDBOX_PRIVILEGED_TESTS=1 SANDBOX_CLOCK_TESTS=1` | root, **a throwaway VM** (it sets that kernel's wall clock; a container shares the host's, so CI doesn't run it) | stepping forward and back with `ADJ_SETOFFSET`, slewing at the kernel's rate without the clock going backwards, a step cancelling a slew |
| Filesystem checkers | `SANDBOX_REQUIRE_FSCK_EROFS=1`, `SANDBOX_REQUIRE_E2FSCK=1` | `erofs-utils`, `e2fsprogs` | the EROFS and ext2 images we write, validated by the official `fsck` tools |
| Runtime with the in-process fake | [`crates/core/runtime/src/tests.rs`](../../crates/core/runtime/src/tests.rs) | Unix | lifecycle, ownership, names, records, egress, `--connect` between sandboxes, `cp` targets, build layers exported as OCI tars and merged by `sbx-rootfs`, the guest clock set after start |
| Executor protocol conformance | [`crates/executor/fake/tests/conformance.rs`](../../crates/executor/fake/tests/conformance.rs) | Unix | `sbx-executor-process` against a protocol-speaking binary, including misbehaving ones |
| Firecracker adapter sessions | [`crates/executor/firecracker/tests/session.rs`](../../crates/executor/firecracker/tests/session.rs) | Unix | the protocol session and Firecracker config, with a shell script standing in for `firecracker` |
| Event log | [`crates/port/events/src/tests.rs`](../../crates/port/events/src/tests.rs) | nothing | concurrent appends, rotation, following across generations, half-written and broken lines |
| CLI output contract | `cargo test -p sbx-cli --lib output` | nothing | that [cli-output.md](cli-output.md) and its JSON Schema match the types in `output.rs` (`SBX_BLESS=1` rewrites them), `--jq` |
| Docker compatibility table | `cargo test -p sbx-cli --lib compat` | nothing | every row of `compat.rs` against clap (a working row's flag exists, a missing one doesn't, every sbx flag has a row) and its evidence test exists; [docker-compatibility.md](../guide/docker-compatibility.md) and the README line are current (`SBX_BLESS=1` rewrites them) |
| TUI without a terminal | [`tui/model/tests.rs`](../../crates/app/cli/src/tui/model/tests.rs), [`tui/view/tests.rs`](../../crates/app/cli/src/tui/view/tests.rs) | nothing | every key's `sbx` command line, filtering, virtual scrolling, drawing (ratatui `TestBackend`) |
| CLI end to end | [`crates/app/cli/tests/e2e.rs`](../../crates/app/cli/tests/e2e.rs) | nothing (Windows skips the parts that need Unix sockets) | the real `sbx` binary: flags, output, exit codes, `--rm`, names, `ps`, `logs` (timestamps, `--since`), `cp`, `events`, `--filter`, `--jq`, every JSON output read back strictly as its type, completion candidates, executor versions and pins, MCP |
| Installer | [`.github/scripts/test-install.sh`](../../.github/scripts/test-install.sh) | Linux or macOS | `install.sh` against a fake release, a fake `cosign` and a fake `sbx`: `--version` and the first line, checksum, every `SBX_VERIFY` value, missing or bad signatures, the identity it asks `cosign` for (the pinned tag), archives missing files, and the executor step (pinned or not, `SBX_EXECUTOR`, keeping an executor you chose, a failed install) |
| Installer pins | `cargo test -p xtask installer` | nothing | `install.sh` and `install.ps1` carry exactly what `installer.env` says; a published installer version is never changed; `install.sh` is the newest version |
| Default executor on first use | `the_default_executor_is_installed_on_first_use` in [`e2e.rs`](../../crates/app/cli/tests/e2e.rs) | Unix | with a local signed repository as `official`: the repository's `default`, one notice on stderr and none on stdout, both ways to turn it off, an unsigned or unreachable repository, two first runs at once |
| Dependency rules | `cargo xtask check-deps` | nothing | layers, per-crate allow lists, forbidden features, module cycles, the map in `crates.md` |
| Fuzzing | [`fuzz/`](../../fuzz), `cargo xtask fuzz` ([below](#fuzzing)) | nightly, `cargo-fuzz` | inputs no hand-written test thought of, in every parser that reads bytes from a guest, a registry, an image or an executor repository |
| Real VMs | `cargo xtask smoke` | an installed executor | output and exit codes, stdin, no network device, fast DNS failure, writable root, detached `exec`/`cp`/`logs`/`stop`/`rm`, `--connect`, allow list; live sharing (`-v` both ways, a host edit reaching the guest's `inotifyd`) only when the executor announces `live_share` |
| Numbers | `cargo xtask bench [--docker]` | an installed executor (and Docker for `--docker`) | regressions on the hot path: `run`, `exec`, parallel runs |

When you fix a bug, add the test at the lowest level that can see it. If it took a real VM to find, ask whether a
unit test or the fake executor could have found it.

### Running against real VMs locally

```sh
cargo xtask dist                         # release sbx with the guest init embedded
sbx executor install wick                # once: any executor, e.g. a published VMM and its adapter (or --from, below)
cargo xtask smoke --sbx target/release/sbx
```

`smoke` and `bench` use the same directories and default executor as `sbx` (`SBX_DATA_DIR`, `SBX_CACHE_DIR`, …).
Point them at scratch directories to keep test sandboxes away from your own. `--no-internet` skips the check that
needs an HTTPS site.

## Fuzzing

[`fuzz/`](../../fuzz) is a [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) crate. It is its own
workspace, outside the main one, because it needs nightly and sanitizers; `cargo test --workspace` and
`cargo xtask check-deps` don't see it. Each target checks invariants, not only "doesn't crash": what decodes
re-encodes to the same bytes, what is shown parses back to the same value, nothing is written outside the destination
directory, version order is a total order. The doc comment at the top of each target says exactly what it checks.

| Target | Input | Code |
| --- | --- | --- |
| `guest_msg` | a message body and a stream of frames from the guest | [`sbx-guest-proto`](../../crates/spec/guest-proto/src/lib.rs) decoders, both directions |
| `copy_from_guest` | a hostile guest's answer to `sbx cp` | `hostfs::receive`, `receive_bytes` in [`sbx-runtime`](../../crates/core/runtime/src/hostfs.rs), into a temp dir next to a directory that must stay untouched |
| `layer_from_guest` | a hostile guest's build layer entries | `layer::receive` in [`sbx-runtime`](../../crates/core/runtime/src/layer.rs): accepted layers are whole tars that the `prepare` path reads and merges, and `.wh.` names come only from whiteouts and opaque directories |
| `copy_into_guest` | the host's side of `sbx cp` into a sandbox | `copy::receive` in [`sbx-guest`](../../crates/app/guest/src/copy.rs), into a temp dir next to a directory that must stay untouched; exactly one `Done` or `Error` |
| `mcp_args` | an agent's tool arguments (JSON) | `mcp::run_args` in [`sbx-cli`](../../crates/app/cli/src/mcp.rs): accepted input becomes only the options it names, never `-v`, `--mount`, `--env-file`, `--entrypoint` or another label |
| `egress_rules` | `--network allow=` rules, a guest's `Connect { host, port }` and its name lookups (`Lookup { name }`) | `AllowRule` in `sbx-model`, [`egress::check`](../../crates/core/runtime/src/egress.rs) (IP hosts only, so no DNS), [`egress::lookup`](../../crates/core/runtime/src/egress.rs) (answers from the list only), `is_public_ip` |
| `shim_log` | a `main.log` file, and guest output written then read back | `LogWriter`, `LogReader` in [`sbx-shim`](../../crates/app/shim/src/log.rs) |
| `oci_documents` | manifest, index and image config JSON, `WWW-Authenticate`, image references; for push, registry error bodies, `Range` and `Location` headers and the chunked-upload state | [`sbx-oci`](../../crates/adapter/oci/src/manifest.rs) (and [`push.rs`](../../crates/adapter/oci/src/push.rs)), and the image's `User`, `StopSignal` and `Env` through [`process`](../../crates/core/runtime/src/process.rs) |
| `image_layer` | a layer blob (plain, gzip or zstd) | the `prepare` path: zstd and tar reading in `sbx-manage`, the merge and the EROFS writer in [`sbx-rootfs`](../../crates/port/rootfs/src/lib.rs) |
| `rootfs_merge` | structured layers (whiteouts, opaque dirs, hard links, devices) and a kernel path | `MergedTree::apply_layer`, `resolve`, `ErofsBuilder` |
| `executor_index` | an executor repository index | `Index::parse`, `available`, `pick` in [`sbx-executor-repo`](../../crates/spec/executor-repo/src/index.rs) |
| `release_index` | the sbx release index (`releases.json`) | `Index::parse`, `status`, `target` in [`sbx-release-index`](../../crates/spec/release-index/src/index.rs): the notice and `sbx upgrade` pick the highest unyanked build for the platform and never downgrade by default |
| `executor_spec` | `NAME@VERSION` (also what `.sbx-executor` holds), versions, repository URLs | `Spec`, `version_cmp`, `version_matches` in [`sbx-executor-install`](../../crates/spec/executor-install/src/lib.rs), `location::repository` |
| `executor_unpack` | a package archive (the target gzips it) | `unpack::unpack` in [`sbx-executor-repo`](../../crates/spec/executor-repo/src/unpack.rs), into a temp dir next to one that must stay untouched |

`sbx-runtime`, `sbx-manage` and `sbx-cli` open their internal readers to the fuzz crate with a `fuzzing` feature;
`check-deps` fails if `sbx-cli` or `sbx-shim` ever enables it on a library they depend on, and release builds never
enable `sbx-cli`'s own.

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
cargo xtask fuzz                          # every target, 60 s each
cargo xtask fuzz --time 600 guest_msg     # one target, longer
cargo xtask fuzz image_layer -- -max_len=65536   # anything after -- goes to libFuzzer
```

`cargo xtask fuzz` runs `cargo +nightly fuzz run <target> fuzz/corpus/<target> fuzz/seeds/<target>` with the
dictionary `fuzz/dict/<target>.dict` (when there is one) and a 30 s limit per input, then prints a table of runs,
coverage and corpus size. `SBX_FUZZ_TOOLCHAIN` picks another toolchain. New inputs go to `fuzz/corpus/` (ignored by
git). A failing input is saved in `fuzz/artifacts/<target>/`; reproduce it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`, then turn it into a unit test in the crate that
owns the code (the fixes found so far each have one) and fix it there.

The checked-in seeds in `fuzz/seeds/` are made by [`fuzz/examples/seeds.rs`](../../fuzz/examples/seeds.rs) with the
real encoders (`encode`, `tar::Builder`, gzip, zstd, the shapes the unit tests use), which also checks that each seed
gets past the first parser. Regenerate them with `cargo xtask fuzz --seeds` after changing a format; CI fails when they
differ from the generator. Targets that take structured input (`copy_from_guest`, `copy_into_guest`, `layer_from_guest`, `rootfs_merge`, `shim_log`) and
`mcp_args` (JSON, guided by its dictionary) have no seeds; libFuzzer grows them from nothing within seconds. When you add a parser that reads untrusted bytes, add a
target (a file in `fuzz/fuzz_targets/` and a `[[bin]]` in `fuzz/Cargo.toml`); CI picks it up by file name.

GitHub's runners are Linux, so CI fuzzes on Linux only. On macOS, `cargo fuzz` works the same on Apple Silicon.

## CI

[`ci.yml`](../../.github/workflows/ci.yml) runs on every push to `main` and `feat/**`, and on pull requests.

| Job | Runs on | Does |
| --- | --- | --- |
| `test` | Linux x86_64 and arm64, macOS, Windows | `fmt --check`, `clippy -D warnings`, `test --no-fail-fast`, `doc -D warnings`, `check-deps`. On Windows also the `install.ps1` checks (`.github/scripts/test-install.ps1`: a fake release and a fake `cosign`, with the real `sbx.exe`). On Linux also `shellcheck` on `install.sh` and `.github/scripts/*.sh`, that the installer version in `installer.env` is either unpublished or published with the same content (`cargo xtask installer --check-published` against this repository's `gh-pages`), both guest initramfs builds, the e2e tests again with the init embedded, and `cargo xtask dist` |
| `privileged` | Linux, in a `--privileged` container | the guest tests that need root and loop devices |
| `vm` | Linux x86_64 with KVM (arm64 runs when the runner has KVM) | builds `sbx`, the adapter and the kernel (cached by its config), and makes a VMM package from upstream Firecracker (checked by its sha256). It installs `sbx` with `install.sh` (`SBX_EXECUTOR=none`), puts the adapter and the VMM package (as `firecracker@1.17.0`) in a local schema 2 `file://` repository, and installs from there the way a user does (`.github/scripts/smoke-local.sh`). It uses neither wick nor the official repository. Then it runs `smoke` and `bench` (the numbers go to the job summary) |

[`fuzz.yml`](../../.github/workflows/fuzz.yml) runs on every push to `main` and `feat/**`, and by hand
(`gh workflow run fuzz.yml -f seconds=1800 [-f targets="guest_msg copy_from_guest"]`).

| Job | Does |
| --- | --- |
| `build` | builds every target once with the pinned nightly and cargo-fuzz (AddressSanitizer, debug assertions) |
| `lint` | `fmt --check` and `clippy -D warnings` on `fuzz/`, and that `fuzz/seeds` matches `cargo xtask fuzz --seeds` |
| `run (<target>)` | one job per target: 60 s on a push (`seconds` by hand), starting from the seeds and the corpus the last run left in the Actions cache, with the same arguments as `cargo xtask fuzz`. A crash, a 30 s hang or running out of memory fails the job and uploads the input as the artifact `fuzz-<target>-artifacts` |

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
the `firecracker` adapter) are published by hand from [toollyst/sandbox-executors](https://github.com/toollyst/sandbox-executors),
usually after a new wick release.

### `sbx`

Pushing `v<version>` runs [`release.yml`](../../.github/workflows/release.yml):

1. **`version`** fails unless the tag matches the workspace version in `Cargo.toml`.
2. **`sbx`** builds `sbx-<os>-<arch>.tar.gz` (+ `.sha256`) for Linux x86_64 and aarch64 (static musl), macOS
   arm64 and Windows x86_64, and `sbx-windows-x86_64.zip` (+ `.sha256`) for `install.ps1`, each with the guest init
   embedded and built with `SBX_RELEASE_BUILD=1` (only such builds run the new-version check).
3. **`smoke`** runs on Linux x86_64 with KVM, the same way as the CI `vm` job: it builds the adapter from this
   commit, makes a VMM package from upstream Firecracker, puts both in a local `file://` repository, and has the new
   `sbx` install `firecracker@<version>` from there before `cargo xtask smoke`. A `sbx` release depends on neither a
   wick release nor what the official repository holds, so `sbx` and the executors can be released in any order. If
   the protocol version changes, publish a matching adapter to the official repository afterwards
   ([Executors](#executors)).
4. **`publish`** signs every tar.gz with the archive key (`.minisig`, secret `SBX_RELEASE_SIGNING_KEY`) and creates
   the GitHub release with the `sbx` archives. It does not publish the install scripts; that is a separate step
   ([Install scripts](#install-scripts)). It adds the release to `gh-pages/releases.json` (`cargo xtask release-index`) and signs that with the
   index key (`release-sign`, secret `SBX_RELEASE_INDEX_KEY`), marking it `security` when the CHANGELOG section names a
   GHSA or CVE. Without the secrets those steps are skipped with a warning, and `sbx upgrade` and the new-version
   notice don't see the release. GitHub Pages serves that branch at <https://toollyst.github.io/sandbox/>.

To cut a release:

1. Bump `version` under `[workspace.package]` in `Cargo.toml` (this updates `Cargo.lock` on the next build, and
   `cargo check` in `fuzz/` updates `fuzz/Cargo.lock`). If the install scripts should install the new release, also
   set `SBX` and bump `INSTALLER_VERSION` in `installer.env` and run `cargo xtask installer`
   ([Install scripts](#install-scripts); the `version` job warns when `SBX` isn't the tag). Rename
   `## Unreleased` in [`CHANGELOG.md`](../../CHANGELOG.md) to `## v0.3.0` (and start a new empty `## Unreleased`),
   and merge it to `main` with CI green. The release notes are that section
   ([`release-notes.sh`](../../.github/scripts/release-notes.sh)), followed by GitHub's generated list; a tag without
   a section fails before anything is built.
2. Optionally, on a Mac with a macOS VMM installed (for example `wick`), run `cargo xtask dist && cargo xtask smoke`.
   CI has no macOS VM run, so this is the only check there; it does not gate the release.
3. Tag and push: `git tag -a v0.3.0 -m "sbx 0.3.0" && git push origin v0.3.0`.
4. Publish the install scripts for it with `installer.yml` ([Install scripts](#install-scripts)), then check the
   result the way a user would: `curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh` with scratch
   `SBX_INSTALL_DIR`, `SBX_DATA_DIR` and `SBX_CACHE_DIR`, then `sbx info` and `cargo xtask smoke --sbx <that sbx>`.

`build` and the attestations run in the reusable [`sbx-build.yml`](../../.github/workflows/sbx-build.yml) (SLSA Build
L3: the provenance is signed as that workflow, not as `release.yml`), and the binaries are built with
`cargo xtask dist --auditable`. `sign` makes the Sigstore bundles as `release.yml`, the identity `install.sh` and
`install.ps1` check. `publish` runs in the `release` environment, so it waits until a maintainer approves the
deployment on the run's page (Settings → Environments → `release`: required reviewers, tags `v*` only). Tags `v*` can
only be created by admins, and `main` and `gh-pages` can't be force-pushed or deleted (Settings → Rules).

A tag whose workflow fails publishes nothing, because `publish` only runs when every build succeeded. Fix the cause,
bump the version and tag again rather than moving a pushed tag. If only the `gh-pages` push fails, the GitHub release
is already up: run `cargo xtask release-index` for it by hand (as the step does) and fix the step on `main`.

**Release index and keys.** [`releases-resign.yml`](../../.github/workflows/releases-resign.yml) re-signs
`releases.json` every Monday (it expires after 30 days) in the `release-index` environment (no approval, `main`
only), which holds only the index key. It changes nothing but `serial` and `expires`, and refuses an index the index
key didn't sign, so editing `gh-pages` by hand can't get anything signed. GitHub turns scheduled workflows off after
60 days without repository activity; if that happens the index expires 30 days later and notices stop (`sbx info`
says so), so run it by hand. A queue of more than one `gh-pages` job (publish, resign) keeps only the newest pending
one; re-run whichever was dropped.

To yank a release, a maintainer edits the index with the offline index key and pushes `gh-pages`:

```sh
git worktree add pages gh-pages
cargo xtask release-yank --index pages/releases.json --version 0.2.2 --advisory "data loss on stop; use 0.2.3" --key release-keys/index-1.key
git -C pages commit -qam "releases.json: yank 0.2.2" && git -C pages push origin gh-pages
```

(`--undo` takes it back.) Every xtask command that edits or signs the index first checks that the current file is
signed by that key and that its `serial` isn't more than a day ahead of now. A yanked release is skipped by
`sbx upgrade` and the notice, and an `sbx` running it is told so on every command. Making the keys, putting them into GitHub and switching them is in
[release-keys.md](release-keys.md).

**Rehearsal.** `gh workflow run release.yml --ref <branch>` runs the same jobs on any branch without a tag. It builds,
runs the smoke test, signs with Sigstore and uploads the result as the artifact `rehearsal`, with the notes from
`## Unreleased` (or from the version's section once it has been renamed for the tag). It creates no GitHub release
and no attestations, skips `publish`, and leaves `gh-pages` alone.

**Actions updates.** Dependabot opens one weekly PR for all GitHub Actions
([`dependabot.yml`](../../.github/dependabot.yml)). A pull request runs only `ci.yml`, so before merging one that
changes `release.yml` or `fuzz.yml`, run both on its branch:

```sh
gh workflow run release.yml --ref <branch>
gh workflow run fuzz.yml --ref <branch> -f seconds=60
```

The steps of `publish` that run only on a tag (attestations, the GitHub release, the `gh-pages` push) are first
exercised by the next real release. After a major update of `actions/checkout` or of an action those steps use, check
them in that release's log. The `gh-pages` step skips the push when `releases.json` didn't change; if its log says
`gh-pages is unchanged`, the push is still unchecked. The same goes for `installer.yml`'s `publish`.

### Install scripts

The install scripts have their own version, separate from `sbx`'s and the executor's. Each version installs one
`sbx` release and one executor (with its adapter) that were tested together, and is published once, as
`install-<version>.sh` and `install-<version>.ps1` on `gh-pages`, and never changed; `install.sh` and `install.ps1`
there are copies of the newest (Pages can't redirect, and `curl | sh` must get the file itself). The pinning is a
recommendation: `sbx` itself knows no executor version, and nothing checks the pair after installing.

[`installer.env`](../../installer.env) is the one place for the values: `INSTALLER_VERSION`, `SBX` (a release,
without the `v`), and `EXECUTOR` and `ADAPTER` (`NAME@VERSION` in the official repository; empty means the script
installs no executor and `sbx` fetches the newest default one on first use). `cargo xtask installer` writes them into
the pin blocks of both scripts; `cargo test` fails while a script differs from `installer.env`.

Publish a new installer version when a new `sbx` or a new default executor should be what people get, or when the
scripts themselves change (a fix, a new OS) with the same versions:

1. Edit `installer.env`: bump `INSTALLER_VERSION` and set what changed. Run `cargo xtask installer`, commit, and merge
   to `main` with CI green. CI fails a change to the scripts that keeps an `INSTALLER_VERSION` already on `gh-pages`.
2. On a Mac, check the pair on macOS (GitHub's runners can't start VMs there): run the new `install.sh` from the
   branch with scratch `SBX_INSTALL_DIR`, `SBX_DATA_DIR` and `SBX_CACHE_DIR`, then `cargo xtask smoke --sbx <it>`.
3. `gh workflow run installer.yml`. It checks the pins and that the `sbx` release exists, runs the script the way a
   user does on Linux x86_64 with KVM (the real release, the executor from the official repository) and, with an
   executor pinned, `cargo xtask smoke`. Then it waits for approval in the `installer` environment (Settings →
   Environments: required reviewers, `main` only) and pushes `install-<version>.*` and the new `install.sh` and
   `install.ps1` to `gh-pages` (`cargo xtask installer --publish`). Re-running it is harmless; it refuses to replace
   a published version with other content.

This is the only workflow that reads the official executor repository. `sbx` releases and CI never wait for it, so
`sbx`, the executors and the installer can each be released in any order.

### Executors

The official executor repository lives in its own repository,
[toollyst/sandbox-executors](https://github.com/toollyst/sandbox-executors), and is served at
<https://toollyst.github.io/sandbox-executors/>. Nothing here depends on it: CI and releases build their own local
repository, and `sbx` works without it once an executor is installed.

Its [`executors.yml`](https://github.com/toollyst/sandbox-executors/blob/main/.github/workflows/executors.yml) (by
hand) takes a [toollyst/wick](https://github.com/toollyst/wick/releases) release and a commit of this repository
(`-f sandbox=v0.2.0`, default `main`), builds the `firecracker` adapter and the kernel from that commit, and uses that
commit's tools:

- `cargo xtask repo-add --schema 2` adds the packages to the index; `--default` on the default executor's packages
  (today `wick`) names it in the index's `default`, which is what `sbx` installs when it has no executor.
- `cargo xtask repo-sign` signs the index with a fresh `serial` and a 90-day `expires`.
- [`.github/scripts/upstream-vmm.sh`](../../.github/scripts/upstream-vmm.sh) packages upstream Firecracker on a
  platform the wick release has no build for.
- [`kernel-notice.sh`](../../.github/scripts/kernel-notice.sh) writes the kernel's `KERNEL-SOURCE.txt`, linking that
  commit.
- [`smoke-local.sh`](../../.github/scripts/smoke-local.sh) runs the real-VM check before upload.

These scripts are this repository's interface to it, so change them compatibly or update both. Versions are wick's
release tags without the `v` (`v1.18.0+wick.1` becomes `wick@1.18.0+wick.1`), and a published platform is never
rebuilt or overwritten.

**Signing.** That repository's secret `EXECUTORS_SIGNING_KEY` (a minisign secret key from `cargo xtask repo-keygen`)
signs its index; the matching public key goes in `OFFICIAL_KEYS` in
[`executors.rs`](../../crates/app/cli/src/executors.rs). Until both exist, `official` is an unsigned repository, and
once `OFFICIAL_KEYS` has a key the workflow refuses to publish unsigned. `sbx` installs the default executor on its
own only from a signed `official`, so a release without a key in `OFFICIAL_KEYS` never does. Its `executors-resign.yml` re-signs before
the index expires. To rotate the key, add the new public key next to the old one in `OFFICIAL_KEYS`, release, then
switch the secret.

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
