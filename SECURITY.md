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
came out. While `sbx` is below 1.0, a minor release (0.2, 0.3, …) may break compatibility, so the fix may require
moving to the newest patch of a supported minor. `main` always has the fix.

## Verifying what you install

From v0.2.0, every release archive comes with a Sigstore signature (`.sigstore.json`), a GitHub build provenance
attestation (SLSA) and an SBOM (SPDX, `sbx-<version>.spdx.json`). `install.sh` checks the signature when `cosign` is
on `PATH` (`SBX_VERIFY=require` makes that mandatory). To check by hand:

```sh
gh attestation verify sbx-linux-x86_64.tar.gz -R toollyst/sandbox
cosign verify-blob --bundle sbx-linux-x86_64.tar.gz.sigstore.json \
  --certificate-identity-regexp '^https://github[.]com/toollyst/sandbox/[.]github/workflows/release[.]yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com sbx-linux-x86_64.tar.gz
```

Dependencies are checked on every push against the RustSec advisory database, an allow list of licenses and
crates.io as the only source ([`deny.toml`](deny.toml)), and GitHub Actions are pinned to commit SHAs.

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
| Guest → host messages | Host code that reads guest messages (exec I/O, file copy, egress) treats them as hostile, and the repository has tests that play a lying guest. Frames are capped at 16 MiB. In `cp` from the guest, the host picks the name of what it writes from the path you asked for (never from the guest's answer), every path must be a safe relative path under it, and a symlink in the middle of a path is an error, so a guest can't write outside the destination you chose. Setuid, setgid and sticky bits are dropped. Output the host hasn't consumed yet is capped (4 MiB per process; the guest waits), and a detached sandbox's `main.log` stops at 256 MiB. |
| Guest → host files | The image's root filesystem is a read-only EROFS disk shared by sandboxes of that image. Writes go to a per-sandbox disk. Host directories are visible to a sandbox only if you pass `-v`/`--mount`, and the Firecracker executor doesn't support live mounts at all (`type=copy` copies at start). |
| Image → host (during pull and prepare) | Layers are verified against their SHA-256 digests, and a download stops as soon as it is larger than the manifest says. They are never unpacked onto the host filesystem by path: file contents are stored under generated names and merged into a single EROFS image file. Symlinks are resolved inside the image only (at most 40 hops). Only the registry host you named may be on a private or local address; token endpoints and redirects the registry hands out must be HTTPS and are resolved to internet addresses only, so a hostile registry can't make `sbx` fetch from your LAN or a metadata endpoint. Stalled reads time out after 60 s. |
| Executor packages → host | Adapters, VMM packages and self-contained executors are checked against the size and SHA-256 in the repository index before they are unpacked. Indexes and packages are fetched over HTTPS (plain HTTP is accepted only from loopback; `file://` from local disk), and redirects follow the same rule. An archive may hold only regular files and directories: links, device files, absolute paths and `..` are refused. Before a self-contained executor or a VMM package is installed, it (for a VMM package, its adapter given that package) must report this `sbx`'s protocol version and the host's architecture; an adapter installed on its own is not checked. That check runs the downloaded program, so it is about compatibility, not security. The `firecracker` adapter writes each VM's Firecracker config next to its control socket, in the socket directory `sbx` keeps for your user only (it refuses one owned by someone else and sets it to `0700`): always a new file, mode `0600`, after removing whatever was at that path, so a link planted there is never followed. A `.sbx-executor` file in the current folder or one above it (say, in a repository you cloned) can only pick among the executors you installed, older versions you kept included (`sbx executor prune` removes those); it can't name a program path or make `sbx` download anything. |
| Local helpers (`events`, `tui`, `completion`, `--jq`) | None of them adds a capability or a background process. `sbx events` has no daemon: the process that changes a sandbox or an image appends one line under `events/` in the data or cache directory (about 2 MiB per directory at most; older lines are dropped), and readers skip lines they can't parse. Events carry IDs, names, image references, exit codes and error messages, never a process's output. `sbx tui` only runs the same `sbx` binary as a child with an argument list, never a shell; its `:` prompt splits words like a shell but has no variables, globs or pipes, and runs only `sbx`. Shell completion (`COMPLETE=<shell> sbx`, which the script from `sbx completion` runs on every Tab) only reads the data and cache directories: it starts no executor and writes nothing. `--jq` runs an expression you wrote on `sbx`'s own JSON output, in-process with the built-in jaq. It has no file or network functions, but it can read `sbx`'s environment (`env`), so treat a `--jq` expression from someone else like any script you run. |
| AI agents (`sbx mcp`) | The MCP server adds no capabilities beyond the CLI. It never mounts host folders, keeps the network off unless a call passes `allow`, and builds arguments so tool input can't become other `sbx` options. Its tools only see sandboxes created through MCP, so an agent can't reach a sandbox you started with `-v` (`sbx mcp --all-sandboxes` widens this). `read_file` reads into memory and never follows a link on the host. Cancelled calls and a vanished client power off and remove what they started. |

### Not covered yet

These are known gaps, in rough order of importance. Some are on the [roadmap](docs/roadmap.md).

- **No jailer.** The VMM runs as your user, without Firecracker's jailer (chroot, namespaces, cgroups). An escape
  from the VM lands as your user on the host.
- **Repository indexes are not signed yet** (planned: minisign signatures with a serial and expiry, see the
  [roadmap](docs/roadmap.md) M7). The SHA-256 in the index protects each package, and TLS is all that protects
  the index, so whoever controls the repository host controls what you install. This holds for adapters and VMM
  packages alike.
- **Names are not tied to a repository.** `sbx executor install NAME` takes the highest version of that name from
  any configured repository (an equal version from the earlier one). The adapter a VMM package needs, if it isn't
  installed yet, comes from the repository that served the VMM package, or else from the first repository in the
  list that has it. But a VMM package always runs under the highest *installed* version of its adapter, wherever
  that came from: an adapter you install with `sbx executor adapter install` from another repository takes over
  every VMM package that names it. Add only repositories you trust as much as the official one.
- **Data you allow out can leave.** The allow list decides *where* a sandbox may connect, not *what* it sends there.
- **Resource limits are coarse.** vCPU count, memory and scratch disk size are fixed per sandbox. There are no CPU
  quotas, I/O limits or caps on how much disk an image pull and prepare may use, and `sbx mcp` does not cap how
  many sandboxes, vCPUs or how much memory an agent asks for.
- **The macOS VMM is young.** See the first boundary above: wick uses Seatbelt in place of seccomp, has no jailer
  equivalent yet, and itself advises against untrusted multi-tenant workloads on macOS until that lands.
- **Side channels** between VMs on the same host (CPU caches, timing) are out of scope.
- **The guest init and its protocol have not been audited by a third party.** The decoders are tested with 200,000 random and mutated messages on every run, but there is no coverage-guided fuzzing yet.
