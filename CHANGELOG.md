# Changelog

What changed in each release, for people who use `sbx` and script against it. The rules for what counts as breaking
are in [COMPATIBILITY.md](COMPATIBILITY.md); fixes for vulnerabilities are listed under **Security**
([SECURITY.md](SECURITY.md)).

Each release has a section `## vX.Y.Z`; the release workflow publishes that section as the release notes and refuses
to release a tag without one. Changes land under `## Unreleased` first, and the section is renamed when tagging.

## Unreleased

### Breaking

- `install.sh` no longer installs an executor. Pick one with `sbx executor search` and `sbx executor install NAME`.
- Executors are split into VMM packages and the adapters that run them, in a version 2 repository index
  (`v2/index.json`). The official repository publishes `wick` (VMM packages) and the `firecracker` adapter.
- `sbx executor repo add` needs `--key KEY` (a signed repository) or `--unsigned`.
- Executor and adapter names and versions are lowercase and don't end in `.`: macOS and Windows file systems treat
  `Fake` and `fake` (and Windows `fake.` and `fake`) as one directory. An executor installed earlier under such a name
  no longer shows up; remove its directory and install it again under a lowercase name. Index entries with such
  names are skipped, not fatal, and `cargo xtask repo-add` refuses them.
- The repository name `local` is reserved for executors installed with `--from`.
- A VMM package's adapter now comes from the first repository that publishes the adapter's name, not from the
  VMM package's repository first; an adapter installed from another repository is not used for it.
- `sbx executor install NAME` refuses to take NAME from a repository when one earlier in `repo ls` order could not be
  read; fix that repository or choose with `--repo`.
- `sbx mcp` caps what agents can ask for: 8 sandboxes at once, 4 vCPUs and 4 GiB of memory each (`--max-sandboxes`,
  `--max-cpus`, `--max-memory`).
- Labels an image declares under `io.github.toollyst.sandbox.` no longer become sandbox labels (the image's
  `io.github.toollyst.sandbox.kernel` still picks its kernel).
- `install.sh`: when `cosign` is on `PATH`, a release without a signature is an error, except a release before
  v0.2.0 named with `SBX_VERSION`.

### Security

- Signed executor repositories: minisign signatures on the index, `serial` against rollbacks, `expires` against
  frozen mirrors. A name belongs to the first repository that publishes it and is only reinstalled from where it came
  from. A name can't be taken from behind a repository that failed to load, or by changing its case.
- A remote index can only point at `https` archives (or ones of its own scheme), and every redirect hop is checked:
  no step from `https` down to `http`, no more than 10.
- Executor indexes have at most 64 archives per version, so a hostile one can't stall `search` and `install`.
- Release archives are signed with Sigstore and come with build provenance and an SBOM; `install.sh` checks the
  signature (`SBX_VERIFY`), and with `SBX_VERSION` it must come from that exact tag.
- MCP: `read_file` reads into memory and never follows a link on the host; tools only see sandboxes MCP created; an
  image can't make its sandbox MCP's; concurrent calls can't exceed the sandbox limit.
- `cp` from a guest: the host picks the top-level name, and names that are paths or devices on the host (`..\x`,
  `C:x`, `NUL`) are refused.
- Guest output can't fill host memory or disk; egress has a cap on concurrent connections; image pulls only follow
  registry-chosen addresses to the internet, with read timeouts and blob size caps.
- Allow rules match IPv6 addresses by value, not spelling.

### Added

- `sbx tui`: sandboxes, images, events and executors on one screen, with keys for `sbx` commands.
- `sbx events`, `--filter` (Docker rules), `--jq`, `sbx logs -t/--since/--until/--format json`, shell completion of
  sandbox names and image references.
- Executor versions the nvm way: a default, `--executor NAME@VERSION`, a `.sbx-executor` file, per-sandbox pins,
  main versions and patches, adapters, `--repo`, the FROM column.
- `cargo xtask repo-keygen` and `repo-sign` for running your own signed repository.
- [COMPATIBILITY.md](COMPATIBILITY.md), [SECURITY.md](SECURITY.md) response times and supported versions,
  governance files, coverage-guided fuzzing of every parser that reads untrusted bytes.

## v0.1.1

First public release.
