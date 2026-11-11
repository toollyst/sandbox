# Security

`sbx` exists to run code you don't trust. This page says what it defends against, how, and what it does not
cover yet. If you find a way across one of the boundaries below, please report it privately.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting: **Security → Report a vulnerability** on this repository. Please
don't open a public issue. Include the output of `sbx info` (version, host, hypervisor and executor) and the smallest
reproduction you have.

**What counts.** Anything that crosses one of the boundaries in the [threat model](#threat-model) below is a
vulnerability and gets a CVE: code in a sandbox reaching the host, another sandbox, the network beyond its allow
list, or files you didn't share; a hostile image, registry or executor repository making `sbx` do more than the
table says; a package or index you didn't approve getting installed. A bug that only affects a sandbox's own
contents is an ordinary bug.

**Where to report.** `sbx`, `sbx-shim`, the guest init, the `firecracker` adapter and the guest kernel build we ship
all live in this repository: report here. Report a flaw in the macOS VMM to [wick](https://github.com/toollyst/wick/security),
and one in upstream Firecracker through [its process](https://github.com/firecracker-microvm/firecracker/security/policy);
if you aren't sure, report here and we will route it.

**What happens next.**

| Step | When |
| --- | --- |
| We confirm we received it | within 48 hours |
| We tell you whether we consider it a vulnerability, and how severe | within 7 days |
| Fix released, advisory (GHSA) published with a CVE, reporter credited unless they ask otherwise | as soon as the fix is ready; at most 14 days of embargo after that, and no later than 90 days after the report |

If a fix needs a change in wick, upstream Firecracker or the Linux kernel, we coordinate with that project and follow
its embargo. Every release note has a **Security** section listing the advisories it fixes.

**Supported versions.** The latest two minor releases get security fixes, each for at least one year after it
came out, starting with 0.2. Releases before 0.2 are withdrawn and get no fixes. While `sbx` is below 1.0, a minor release (0.2, 0.3, …) may break compatibility, so the fix may require
moving to the newest patch of a supported minor. `main` always has the fix.

## Verifying what you install

From v0.2.0, every release archive comes with a Sigstore signature (`.sigstore.json`), a GitHub build provenance
attestation (SLSA) and an SBOM (SPDX, `sbx-<version>.spdx.json`). `install.sh` checks the signature when `cosign` is
on `PATH` (`SBX_VERIFY=require` makes that mandatory), and so does `install.ps1` on Windows
(`sbx-windows-x86_64.zip`). Then a missing signature is an error too, since whoever
controls the download can delete it. With
`SBX_VERSION`, the signature must come from that exact tag, so an older signed release can't be served in its
place. To check by hand:

```sh
gh attestation verify sbx-linux-x86_64.tar.gz -R toollyst/sandbox
cosign verify-blob --bundle sbx-linux-x86_64.tar.gz.sigstore.json \
  --certificate-identity-regexp '^https://github[.]com/toollyst/sandbox/[.]github/workflows/release[.]yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com sbx-linux-x86_64.tar.gz
```

From v0.2.1, the archives are built and attested inside a reusable workflow,
[`sbx-build.yml`](.github/workflows/sbx-build.yml), which the release workflow can call but not change (SLSA Build
Level 3). Add `--signer-workflow toollyst/sandbox/.github/workflows/sbx-build.yml` to `gh attestation verify` to
require that. The binaries also carry their dependency list ([cargo-auditable](https://github.com/rust-secure-code/cargo-auditable)),
so `cargo audit bin sbx`, syft or Trivy can check an installed `sbx` without the source. Publishing a release needs a
maintainer's approval (the `release` environment), and release tags and `main` are protected by repository rulesets.

Dependencies are checked on every push against the RustSec advisory database, an allow list of licenses and
crates.io as the only source ([`deny.toml`](deny.toml)), and GitHub Actions are pinned to commit SHAs.

**Findings.** An advisory or license violation fails CI, so it is dealt with before the change merges: upgrade or
replace the crate, or, if `sbx` can't reach the affected code, ignore that advisory in `deny.toml` with the reason and
a tracking issue. No release ships with an open one that affects `sbx`. CodeQL (the Rust code and the workflows) and
Scorecard report to code scanning: high and critical alerts are fixed, or dismissed with a written reason, before the
next release; others within 90 days. Secret scanning with push protection is on for the repository.

## Threat model

**Assumed hostile:** everything inside a sandbox. That includes root in the guest, the image's contents, and a
guest kernel that has been taken over. The image itself (its layers and config, as pulled from a registry) is
also untrusted input to the host.

**Trusted:** the host, the user running `sbx`, the `sbx` binaries, and the executors you install: an adapter (for
example `firecracker`, with its default kernel), a VMM package it runs (for example `wick`), or a self-contained
executor. They run as your user, outside any VM. So is every executor repository you configure, for every name it
publishes (see below).

### Boundaries, and how each is held

| Boundary | What holds it |
| --- | --- |
| Guest → host CPU and memory | Hardware virtualization (KVM on Linux, Hypervisor.framework on macOS) through the VMM. Each sandbox has its own kernel. On Linux the VMM is upstream Firecracker with its default seccomp filters on (`sbx` never passes `--no-seccomp`). On macOS it is [wick](https://github.com/toollyst/wick), Firecracker built from upstream's source on Hypervisor.framework. macOS has no seccomp; wick confines the VMM with a Seatbelt sandbox instead, its jailer equivalents are still in progress, and its platform code has not had the review upstream has had ([wick's security model](https://github.com/toollyst/wick/blob/main/SECURITY.md)). |
| Guest → host network | The VM has **no network device**. With `--network allow`, a proxy inside the guest hands each connection to the host over vsock. The host checks `host:port` against your list, resolves names itself, and refuses names that resolve to anything but internet addresses (loopback, private, link-local, CGNAT, reserved, documentation, IPv6 unique-local and site-local, and IPv4 inside NAT64 or 6to4 addresses judged by that IPv4) unless the rule names that IP or `localhost`. This blocks DNS rebinding onto your LAN or cloud metadata endpoints. A sandbox can hold at most 256 connections open at once. |
| Guest → host messages | Host code that reads guest messages (exec I/O, file copy, egress) treats them as hostile, and the repository has tests and fuzz targets that play a lying guest. Frames are capped at 16 MiB. In `cp` from the guest, the host picks the name of what it writes from the path you asked for (never from the guest's answer), every path must be a safe relative path under it, and a symlink in the middle of a path is an error, so a guest can't write outside the destination you chose. Setuid, setgid and sticky bits are dropped. Output the host hasn't consumed yet is capped (4 MiB per process; the guest waits), and a detached sandbox's `main.log` stops at 256 MiB. |
| Guest → host files | The image's root filesystem is a read-only EROFS disk shared by sandboxes of that image. Writes go to a per-sandbox disk. Host directories are visible to a sandbox only if you pass `-v`/`--mount`, and the Firecracker executor doesn't support live mounts at all (`type=copy` copies at start). |
| Image → host (during pull and prepare) | Layers are verified against their SHA-256 digests, and a download stops as soon as it is larger than the manifest says. They are never unpacked onto the host filesystem by path: file contents are stored under generated names and merged into a single EROFS image file. Symlinks are resolved inside the image only (at most 40 hops). Only the registry host you named may be on a private or local address; token endpoints and redirects the registry hands out must be HTTPS and are resolved to internet addresses only, so a hostile registry can't make `sbx` fetch from your LAN or a metadata endpoint. Stalled reads time out after 60 s. Registry credentials (Docker's `config.json`, its credential helpers, containers `auth.json`) are looked up only for the registry you named and only when it asks for them; they go to that registry or the token endpoint it names, never over plain HTTP (except a `localhost` registry), and redirects to other hosts drop them. `sbx login` writes the same files with mode 0600 and never takes a password on the command line. |
| Executor packages → host | A repository added with `--key` is a **signed repository**: its index must carry a minisign signature by one of its keys, a `serial` and an `expires`, and `sbx` refuses an index that doesn't verify, has expired (a frozen mirror) or has a lower serial than one it already saw from that repository (a rollback; seen serials are kept in `executor-repositories.seen`). A repository added with `--unsigned` relies on TLS, and `sbx` warns each time it reads one. **A name belongs to one repository**: `install NAME` takes it from the first repository in priority order that publishes it, and an installed name (executor or adapter) only comes from the repository it came from (recorded next to it, shown as FROM), so a repository added later can't take over an executor. Adapter names follow the same rule: a VMM package's adapter comes only from the first repository that publishes that adapter name, and one installed from another repository isn't used for it, so a repository can't ship its own `firecracker` alongside its VMM packages and have it run everyone's. If a repository earlier in the list can't be read (blocked, tampered with, rolled back, expired), `install` refuses to take a name from a later one, so making the owner fail doesn't hand its names to someone else. Names and versions are lowercase, because macOS and Windows file systems would treat `Fake` and `fake` as one directory. The repository name `local` is reserved for direct installs. Marker files a package ships itself (`.sbx-origin`, `.sbx-adapter`) are removed before install. Adapters, VMM packages and self-contained executors are checked against the size and SHA-256 in the repository index before they are unpacked. Indexes and packages are fetched over HTTPS (plain HTTP is accepted only from loopback; `file://` from local disk), and redirects follow the same rule. An archive may hold only regular files and directories: links, device files, absolute paths and `..` are refused. Before a self-contained executor or a VMM package is installed, it (for a VMM package, its adapter given that package) must report this `sbx`'s protocol version and the host's architecture; an adapter installed on its own is not checked. That check runs the downloaded program, so it is about compatibility, not security. The `firecracker` adapter writes each VM's Firecracker config next to its control socket, in the socket directory `sbx` keeps for your user only (it refuses one owned by someone else and sets it to `0700`): always a new file, mode `0600`, after removing whatever was at that path, so a link planted there is never followed. A `.sbx-executor` file in the current folder or one above it (say, in a repository you cloned) can only pick among the executors you installed, older versions you kept included (`sbx executor prune` removes those); it can't name a program path or make `sbx` download anything. |
| Local helpers (`events`, `tui`, `completion`, `--jq`) | None of them adds a capability or a background process. `sbx events` has no daemon: the process that changes a sandbox or an image appends one line under `events/` in the data or cache directory (about 2 MiB per directory at most; older lines are dropped), and readers skip lines they can't parse. Events carry IDs, names, image references, exit codes and error messages, never a process's output. `sbx tui` only runs the same `sbx` binary as a child with an argument list, never a shell; its `:` prompt splits words like a shell but has no variables, globs or pipes, and runs only `sbx`. Shell completion (`COMPLETE=<shell> sbx`, which the script from `sbx completion` runs on every Tab) only reads the data and cache directories: it starts no executor and writes nothing. `--jq` runs an expression you wrote on `sbx`'s own JSON output, in-process with the built-in jaq. It has no file or network functions, but it can read `sbx`'s environment (`env`), so treat a `--jq` expression from someone else like any script you run. |
| AI agents (`sbx mcp`) | The MCP server adds no capabilities beyond the CLI (so images are pulled with your registry credentials: an agent can run a private image you can pull, but never sees the credentials). It never mounts host folders, keeps the network off unless a call passes `allow` (and refuses rules for loopback, private or link-local addresses and `localhost` unless the server runs with `--allow-private-network`), and builds arguments so tool input can't become other `sbx` options. Its tools only see sandboxes created through MCP, so an agent can't reach a sandbox you started with `-v` (`sbx mcp --all-sandboxes` widens this). Which sandboxes are MCP's is a label only `sbx` sets: labels an image declares under `io.github.toollyst.sandbox.` are dropped, so an image can't hand its sandbox to MCP or pin its executor. `read_file` reads into memory and never follows a link on the host. Cancelled calls and a vanished client power off and remove what they started. An agent can't ask for more than the server allows: by default 8 sandboxes at once (counting those this server is still starting, and the MCP sandboxes other servers on the same data directory already made; two servers creating at the same moment can each take the last place), 4 vCPUs, 4 GiB of memory and a 16 GiB writable disk per sandbox (`--max-sandboxes`, `--max-cpus`, `--max-memory`, `--max-disk`), and `--max-lifetime` stops each one after a set time. |

### Not covered yet

These are known gaps, in rough order of importance. Some are on the [roadmap](docs/roadmap.md).

- **Confining the VMM is the executor's job.** `sbx` talks to executors over the executor protocol and doesn't
  confine the VMM process itself. The `firecracker` adapter runs it as your user, with Firecracker's seccomp filters
  but without a jailer, so an escape from the VM lands as your user on the host.
- **The official repository is not signed yet.** Signed repositories work (see above), but the official
  repository's key hasn't been created, so `official` is still an unsigned repository: TLS is all that protects its
  index, and whoever controls [toollyst/sandbox-executors](https://github.com/toollyst/sandbox-executors) (its
  Pages and Releases) controls what `sbx executor install` gets from it.
- **A repository you add is trusted for the names it publishes first.** If you add a repository ahead of
  `official` in the list file, or install a name from it with `--repo` before anything else has it, that repository
  owns the name on your machine.
- **Data you allow out can leave.** The allow list decides *where* a sandbox may connect, not *what* it sends there.
- **Resource limits are coarse.** vCPU count, memory and writable disk size (`--storage-opt size=`) are set per sandbox. There are no CPU
  quotas, I/O limits or caps on how much disk an image pull and prepare may use. `--timeout` stops a sandbox after a set time. `sbx mcp` caps how many
  sandboxes an agent runs, how big each one is and, with `--max-lifetime`, how long it runs, but not how much it
  sends out.
- **The macOS VMM is young.** See the first boundary above: wick uses Seatbelt in place of seccomp, has no jailer
  equivalent yet, and itself advises against untrusted multi-tenant workloads on macOS until that lands.
- **An image can pick its own kernel.** An image that sets the label `io.github.toollyst.sandbox.kernel` boots the
  kernel it carries instead of the executor's default, when the executor supports custom kernels. That doesn't cross
  the boundary (the guest kernel is assumed hostile anyway), but it drops whatever hardening the default kernel has
  and hands the VMM's kernel loader a file the image chose. Use images you trust for that, or an executor without
  `custom_kernel`.
- **Side channels** between VMs on the same host (CPU caches, timing) are out of scope.
- **The guest init and its protocol have not been audited by a third party.** The host-side parsers of untrusted
  input (guest messages and `cp` answers, egress requests, the detached log, registry documents, image layers,
  executor indexes and packages) are fuzzed with coverage guidance ([`fuzz/`](fuzz), cargo-fuzz), but CI gives each
  target only a minute per push, on Linux. The guest init's own handling of requests inside the VM is not fuzzed.
