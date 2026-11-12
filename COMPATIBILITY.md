# Compatibility

What you can rely on across `sbx` releases, and what may change. Security support windows are in
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
| Executor protocol | [`sbx-executor-proto`](crates/spec/executor-proto/src/lib.rs) | Versioned. An executor reports the range of versions it speaks, `sbx` speaks a range too, and they use the highest common one. A new `sbx` keeps speaking the previous protocol version for at least one minor release (and at least 12 months), so installed adapters and executors keep working while their authors catch up. |
| Executor repository index | [`sbx-executor-repo`](crates/spec/executor-repo/src/lib.rs) | Versioned by `schema`. Within a schema, fields are only added and clients ignore unknown ones. A new schema is published next to the old one (`v2/index.json` next to `index.json`), and the old one keeps being served for the releases that read it. |
| sbx release index (`releases.json`, read by the new-version notice and `sbx upgrade`) | [`sbx-release-index`](crates/spec/release-index/src/lib.rs) | Versioned by `schema`. Within a schema, fields are only added and clients ignore unknown ones; an `sbx` that doesn't read a newer schema stays quiet instead of guessing. |
| Data and cache directories | [how it works](docs/developers/how-it-works.md) | A newer `sbx` reads what an older one wrote, migrating it in place if needed. An older `sbx` refuses a directory a newer one changed instead of guessing (the `layout-version` file in each, from 0.2.1). Neither silently drops sandboxes, images or installed executors. |
| `.sbx-executor` files, image and sandbox labels (`io.github.toollyst.sandbox.*`) | [`choose.rs`](crates/app/cli/src/choose.rs), [executors guide](docs/guide/executors.md) | Same rules as the CLI. |
| Rust crates in this repository | — | Not published and not a contract (`publish = false`). Use the CLI, its JSON output or the protocols. |

The guest protocol between `sbx` and its guest init is internal: the guest init is embedded in each `sbx` build, so
both sides always match.

## Platforms and toolchain

The supported hosts are listed in the README's [Platforms](README.md#platforms) table. Dropping one is breaking.
The minimum supported Rust version is `rust-version` in [`Cargo.toml`](Cargo.toml); raising it is allowed in a minor
release and noted in the release notes.
