# Changelog

What changed in each release, for people who use `sbx` and script against it. The rules for what counts as breaking
are in [COMPATIBILITY.md](COMPATIBILITY.md); fixes for vulnerabilities are listed under **Security**
([SECURITY.md](SECURITY.md)).

Each release has a section `## vX.Y.Z`; the release workflow publishes that section as the release notes and refuses
to release a tag without one. Changes land under `## Unreleased` first, and the section is renamed when tagging.

## v0.2.1

Nothing that worked in 0.2.0 stops working because of a contract change: commands, flags, JSON fields and MCP tools
are only added. A few defaults are now safer, and `sbx` now honors the containers configuration files that podman and
skopeo already use; both are listed under **Changed** with what to do.

### Security

- A guest that accepts a connection and then stops answering can no longer hang `stop`, `rm -f`, the shim or MCP:
  every request–response step with the guest waits at most 10 seconds.
- A layer with a huge GNU long name or PAX header can't make `sbx` read it into memory: extension headers over 1 MiB
  are refused before they are read, with limits on paths, links and xattrs.
- Sandbox records (`spec.json` with `-e` values, logs, the writable disk) are readable by you only (0700/0600); the
  `sandboxes/` directory 0.2.0 made is narrowed on first use.
- `cp` out of a sandbox drops group- and world-write bits; `cp` in never follows a link or opens a FIFO that
  replaced a file after it was checked.
- `sbx mcp` refuses `allow` rules that open loopback, private or link-local addresses or `localhost`, so an agent
  can't reach services on this machine, your LAN or cloud metadata (`--allow-private-network` restores the old
  behavior).
- The executor log (including the guest serial console) is capped at 16 MiB per VM.
- Executor repository downloads time out (30 s to connect, 60 s without data).
- Releases are built by a reusable workflow with SLSA build provenance and an SBOM attestation, binaries carry their
  dependency list (`cargo auditable`), and publishing needs approval in a protected environment. CodeQL, secret
  scanning and push protection run on the repository; see the
  [OSPS Baseline self-assessment](docs/developers/osps-baseline.md).

### Changed

- `sbx` reads the containers [`policy.json`](docs/guide/usage.md#images) (`~/.config/containers/policy.json`, else
  `/etc/containers/policy.json`) and refuses what it rejects. Without one, everything is accepted as before. On
  Fedora and RHEL the default file asks for GPG signatures on Red Hat's registries, which `sbx` can't check, so those
  images now fail to pull with a message saying so; add an `insecureAcceptAnything` scope for them to pull them
  without a check.
- `sbx` reads the containers [`registries.conf`](docs/guide/usage.md#images): mirrors, rewritten locations, blocked
  registries and `insecure` registries now apply to `pull` and `login`.
- Releases before 0.2 are withdrawn: their GitHub releases, tags and the old executor index are gone, and nothing
  reads what they wrote. An `official` line with the 0.1.x URL in a repository list is no longer read as the
  official repository, and `install.sh` has no exception for unsigned releases.
- The data and cache directories get a `layout-version` file; a later `sbx` that changes their layout migrates them,
  and an older one refuses instead of guessing.
- `sbx --help` groups commands by what you want to do (sandboxes, working inside one, images, watching and
  automating, setup) and ends with first steps. `--help` is as compact as `-h` and wraps to the terminal. `run` and
  `create` group their options (process, resources, files, network, stopping).
- `sbx tui`: errors read as sentences, not JSON; an empty tab says what to do next in its middle instead of a
  warning; Enter shows a row's fields as `name  value` lines (with the `--format json` command that prints them);
  the bottom line only lists keys that do something now; `?` groups keys by tab and scrolls.
- Release builds use fat LTO and are stripped (the macOS arm64 binary went from 16.7 MB to 9.7 MB). `sbx events`
  and `sbx tui` read long event logs in linear time.

### Added

- `-p`/`--publish [IP:][HOST_PORT:]PORT` on `run` and `create`: publish a sandbox TCP port on the host. It binds to
  127.0.0.1 unless you name an address (Docker binds to every address), works with the network off, and `ps`/`inspect`
  show the port picked for `-p PORT`.
- `sbx load` and `sbx save`: move images as OCI image layout archives without a registry. `load` reads
  `docker save` (Docker 25+), `podman save --format oci-archive` and `skopeo` archives, gzipped or not, from a file,
  a directory or stdin; `save` writes what was pulled, unchanged, so `docker load`, `podman load` and `sbx load` give
  the same image ID. Images pulled before 0.2.1 must be pulled again before they can be saved.
- Private registries: `sbx` uses Docker's `config.json` (with `credHelpers` and `credsStore`), `REGISTRY_AUTH_FILE`
  and the containers `auth.json`. `sbx login` and `sbx logout` write and remove the same entries.
- Image signatures: `sigstoreSigned` requirements with keys in `policy.json` check cosign key signatures (ECDSA,
  RSA, Ed25519) before any layer is downloaded; `signedIdentity` works as in containers/image.
- `--timeout DURATION` on `run`/`create` stops a sandbox that long after each start; MCP `create` takes `lifetime`
  and `sbx mcp --max-lifetime` caps it.
- `--storage-opt size=SIZE` sets the writable disk size; MCP takes `disk` and `sbx mcp --max-disk` caps it.
- `--egress-limit SIZE` caps the bytes a sandbox's outgoing connections move per start; `sbx mcp --max-egress`
  applies a cap to agents' sandboxes.
- `sbx mcp --max-image-size SIZE` refuses images larger than that before downloading them.
- `exec -d` runs a command in the background; `exec --timeout DURATION` kills it when it runs too long (exit 124).
- `sbx stats [--no-stream]`: CPU, memory, disk and process count from inside the guest.
- `sbx system df` and `sbx system prune [-a] [-f] [--filter until=,label=]`.
- Events: `exec_start` and `exec_die` for every exec, and `network` events (`connect`, `refuse`, `close` with bytes)
  for every outgoing connection, kept apart from the lifecycle events (`sbx events --filter type=network`).
- `images --format json` lists the ports and volumes an image declares (`exposed_ports`, `volumes`).
- Executor repositories can mark versions `yanked` and attach an `advisory`; `install` skips yanked versions unless
  you name one, and `executor ls`, `adapter ls` and the TUI show advisories for what you installed.
- Every MCP tool declares an `outputSchema` and annotations (read-only, destructive, idempotent, open-world).
- `sbx tui` keys for the new commands: `E` (`exec -d`), `T` (`stats`) on sandboxes; `U` (`system df`), `P`
  (`system prune -a`), `L` (`login`), `I` (`load`), `O` (`save`) on images.
- Windows x86_64 releases (`sbx-windows-x86_64.zip`, signed like the others) and `install.ps1`
  (`irm https://toollyst.github.io/sandbox/install.ps1 | iex`), into `%LOCALAPPDATA%\Programs\sbx`. `pull`, `prepare`
  and `images` work on Windows; there is no tested executor for it yet.
- `cargo xtask install`: build and install `sbx` where the installers put it, instead of running it from the build
  directory.

### Notes

- New messages in the guest protocol (`exec -d`, `stats`, `-p`) are additions: a sandbox started by 0.2.0 keeps
  working, and only those features ask you to restart it.

## v0.2.0

### Breaking

- `install.sh` no longer installs an executor. Pick one with `sbx executor search` and `sbx executor install NAME`.
- Executors are split into VMM packages and the adapters that run them, in a version 2 repository index
  (`v2/index.json`). `wick` (VMM packages) and the `firecracker` adapter are published to the official repository.
- `sbx executor repo add` needs `--key KEY` (a signed repository) or `--unsigned`.
- Executor and adapter names and versions are lowercase and don't end in `.`: macOS and Windows file systems treat
  `Fake` and `fake` (and Windows `fake.` and `fake`) as one directory. An executor installed earlier under such a name
  no longer shows up; remove its directory and install it again under a lowercase name. Index entries with such
  names are skipped, not fatal, and `cargo xtask repo-add` refuses them.
- The official executor repository moved to <https://toollyst.github.io/sandbox-executors/>, published from
  [toollyst/sandbox-executors](https://github.com/toollyst/sandbox-executors). An `official` line with the old URL in
  a repository list written by 0.1.x reads as the new one; the old URL keeps the schema 1 index for 0.1.x.
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
- `sbx` waits at most 30 seconds for an executor's `capabilities` and reads at most 2 MiB of it, so a hung executor
  can't block installs or `run`.
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
- A user guide in [docs/guide/](docs/guide/) (install per OS, usage, MCP, executors); the README is now the short version.
- [COMPATIBILITY.md](COMPATIBILITY.md), [SECURITY.md](SECURITY.md) response times and supported versions,
  governance files, coverage-guided fuzzing of every parser that reads untrusted bytes.
