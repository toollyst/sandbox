# Compatibility across sbx releases

What you can rely on across `sbx` releases, and what may change. How `sbx` compares with Docker, Podman, nerdctl
and Docker Desktop is a different page: [tool-compatibility.md](docs/guide/tool-compatibility.md). Security support windows are in
[SECURITY.md](SECURITY.md#reporting-a-vulnerability).

## Versions

`sbx` follows [semantic versioning](https://semver.org/). Before 1.0, a **minor** release (0.2, 0.3, …) may break a
contract below, and a **patch** release (0.2.1) never does. From 1.0, only a major release breaks one. Every break is
listed under **Breaking** in the release notes ([CHANGELOG.md](CHANGELOG.md)), with what to do.

A contract is removed or changed only after it has been **deprecated** for at least one minor release: `sbx` keeps
the old behavior and warns on stderr when it is used, and the release notes name the replacement.

## Contracts

| Contract | Where it is defined | Rule |
| --- | --- | --- |
| Commands, flags and exit codes | `sbx --help`, [`cli.rs`](crates/app/cli/src/cli.rs), [`behavior.rs`](crates/app/cli/src/behavior.rs) | New commands and flags may appear in any release. Removing or renaming one, or changing what an exit code means, is breaking. Exit codes 125, 126 and 127 mean what they mean in Docker; `upgrade --check` exits 10 when a newer release exists. Human-readable output (tables, messages) is not a contract; script against `--format json`. |
| JSON output (`--format json`) and `--jq` | [`cli-output.md`](docs/developers/cli-output.md), [`cli-output.schema.json`](docs/developers/cli-output.schema.json) (generated, checked in CI) | Fields are only added. Removing a field, renaming it or changing its type is breaking. Consumers must ignore fields they don't know. |
| MCP tools (`sbx mcp`) | the tool list the server reports | Tools and optional arguments are only added. Removing a tool, renaming an argument or making one required is breaking. |
| Executor protocol | [`sbx-executor-proto`](crates/spec/executor-proto/src/lib.rs) | Versioned. An executor reports the range of versions it speaks, `sbx` speaks a range too, and they use the highest common one. A new `sbx` keeps speaking the previous protocol version for at least one minor release (and at least 12 months), so installed adapters and executors keep working while their authors catch up. `sbx` 0.3 speaks versions 2 and 3. Version 3 (new in 0.3) adds the disk role `volume`; with an executor that speaks only 2, everything but volumes works. Optional features an executor announces in `capabilities` (such as `live_share`) are not versions: without one, only the commands that need it fail. |
| Executor repository index | [`sbx-executor-repo`](crates/spec/executor-repo/src/lib.rs) | Versioned by `schema`. Within a schema, fields are only added and clients ignore unknown ones. A new schema is published next to the old one (`v2/index.json` next to `index.json`), and the old one keeps being served for the releases that read it. |
| sbx release index (`releases.json`, read by the new-version notice and `sbx upgrade`) | [`sbx-release-index`](crates/spec/release-index/src/lib.rs) | Versioned by `schema`. Within a schema, fields are only added and clients ignore unknown ones; an `sbx` that doesn't read a newer schema stays quiet instead of guessing. |
| Data and cache directories | [how it works](docs/developers/how-it-works.md) | A newer `sbx` reads what an older one wrote, migrating it in place if needed. An older `sbx` refuses a directory a newer one changed instead of guessing (the `layout-version` file in each, from 0.2.1). Neither silently drops sandboxes, images or installed executors. Layout 2 (new in 0.3) stores image root filesystems per layer, and `sbx` 0.3 writes it to both directories, so `sbx` 0.2.x refuses both once 0.3 has used them: going back to 0.2.x means other directories (`SBX_DATA_DIR`, `SBX_CACHE_DIR`). Volumes (`data_dir/volumes/`) and the record of where an image was built (an optional field in each image's record) are part of layout 2; since 0.2.x refuses a layout 2 data directory, it never starts a sandbox without the volume it names. |
| `.sbx-executor` files, image and sandbox labels (`io.github.toollyst.sandbox.*`) | [`choose.rs`](crates/app/cli/src/choose.rs), [executors guide](docs/guide/executors.md) | Same rules as the CLI. |
| Rust crates in this repository | — | Not published and not a contract (`publish = false`). Use the CLI, its JSON output or the protocols. |

The guest protocol between `sbx` and its guest init ([`sbx-guest-proto`](crates/spec/guest-proto/src/lib.rs)) is
internal. The guest init is embedded in each `sbx` build, so a sandbox runs the guest init of the `sbx` that started
it. A newer `sbx` adds messages without changing the protocol version (2 in 0.2 and 0.3), so `exec`, `cp`, `logs`,
`stop` and `rm` keep working on a sandbox an older `sbx` started; a feature that needs a newer guest (such as clock
correction) starts working when the sandbox is restarted.

## Platforms and toolchain

The supported hosts are listed in the README's [Platforms](README.md#platforms) table. Dropping one is breaking.
The minimum supported Rust version is `rust-version` in [`Cargo.toml`](Cargo.toml); raising it is allowed in a minor
release and noted in the release notes.
