<h1 align="center">sbx</h1>

<p align="center">
  <b>Run any Docker image in its own microVM.</b><br>
  The commands you already know. A real kernel boundary. About 120 ms from <code>run</code> to done.
</p>

<p align="center">
  <a href="https://github.com/toollyst/sandbox/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/toollyst/sandbox?color=2ea44f"></a>
  <a href="https://github.com/toollyst/sandbox/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/toollyst/sandbox/actions/workflows/ci.yml/badge.svg"></a>
  <a href="#license"><img alt="License: Apache-2.0 OR MIT" src="https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue"></a>
  <img alt="Runs sandboxes on macOS (Apple Silicon) and Linux" src="https://img.shields.io/badge/runs%20on-macOS%20%C2%B7%20Linux-lightgrey">
  <img alt="Status: 0.x" src="https://img.shields.io/badge/status-0.x-orange">
  <a href="https://scorecard.dev/viewer/?uri=github.com/toollyst/sandbox"><img alt="OpenSSF Scorecard" src="https://api.scorecard.dev/projects/github.com/toollyst/sandbox/badge"></a>
</p>

> [!WARNING]
> **`sbx` is 0.x.** It works end to end on real VMs and is maintained from 0.2 on, but it has not had an outside
> security audit. Until 1.0, a minor release (0.3, 0.4, …) may change commands, flags, the on-disk layout or the
> executor protocol, only after a deprecation and always listed in the [CHANGELOG](CHANGELOG.md)
> ([COMPATIBILITY.md](COMPATIBILITY.md)). Read [SECURITY.md](SECURITY.md) for what it does and doesn't defend
> against before you trust it with anything that matters.

```sh
curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
sbx executor install wick          # a VMM to run sandboxes with; any executor that speaks the open protocol works
sbx run --rm -it python:3.12-alpine python
```

<p align="center">
  <img src="assets/demo.svg" width="876" alt="Terminal recording: sbx run prints Linux 6.1.155; time shows how long one run takes; the sandbox has no network device; with --network allow only example.com is reachable and github.com gets 403; a detached sandbox is exec'd into, listed with ps and removed; a Python image runs.">
</p>

<p align="center"><sub>A real recording on an Apple Silicon Mac (<code>cargo xtask demo</code>). Every command ran for real and the
output appears when it actually arrived. Only the typing is simulated.</sub></p>

`sbx` takes an OCI image from Docker Hub, GHCR or your own registry and boots it in a
[Firecracker](https://firecracker-microvm.github.io/) microVM with its own Linux kernel, behind hardware
virtualization. It is made for running code you don't trust: **AI agents** (with a built-in
[MCP server](docs/guide/mcp.md)), user-submitted code, CI jobs and plugins.

## What we're after

- **A real boundary for code you didn't write.** Every sandbox is a VM with its own kernel, behind KVM or
  Hypervisor.framework. A container shares the host's kernel; `sbx` doesn't.
- **Nothing in the way.** One binary: no daemon, no root, no account. On macOS it needs no privileges at all.
- **Offline unless you say so.** The VM has no network device. You allow `host:port` pairs one by one, and the host
  enforces them.
- **Docker muscle memory.** `run -it`, `-d`, `-p`, `exec`, `cp`, `logs -f`, `ps -aq`, `stats`, `login`, `save`/`load`,
  `--name`, `--env-file` and exit codes 125/126/127 behave the way you expect, on the same OCI images, unmodified. Your
  registry credentials, mirrors and signature policy come from the files Docker and podman already read.
- **Open standards, not one VMM.** `sbx` doesn't build in a hypervisor. It talks to an **executor** over a documented
  protocol, installs executors from static repositories that follow an open spec, and runs whatever you installed.
  `wick` is our Firecracker build and one executor among any others; you can write your own and publish your own
  repository, and `sbx` treats them exactly like ours. Installed executors keep working if a repository goes away.
- **Mechanism, not policy.** `sbx` holds the line you draw (the allow list, CPU and memory, which sandboxes an agent
  can see) and leaves the drawing to you.
- **Checkable.** Releases from v0.2.0 on are signed with Sigstore and come with build provenance and an SBOM. Repository indexes are
  signed with minisign, against tampering, rollbacks and frozen mirrors. The [threat model](SECURITY.md#threat-model)
  says what is covered and what isn't yet.

## sbx next to a container

|  | `docker run` | `sbx run` |
| --- | --- | --- |
| Isolation | Namespaces and cgroups on the **host kernel** | A **separate kernel** behind KVM or Hypervisor.framework |
| Network | A bridge network, on by default | **No network device.** Opt in per `host:port`, checked on the host |
| Root filesystem | Writable layer over the image | Read-only EROFS image under a **per-sandbox disk** |
| Runs as | A long-running daemon, usually root | Your user. Per sandbox: the VMM, plus a small `sbx-shim` when detached |
| Images | OCI | The same OCI images, unmodified |
| Commands | `docker …` | `sbx …`, with the same names and flags |

`cargo xtask bench` on an Apple Silicon Mac, 1 vCPU and 512 MiB per sandbox: `sbx run --rm alpine:3.20 true` (boot,
run, shut down, delete) takes **121 ms** at p50, `sbx exec` into a running sandbox **8 ms**, and ten runs at once
finish in **0.50 s**. Try it on your machine ([source](xtask/src/bench.rs)).

## Install

> [!NOTE]
> The official executor repository has nothing published yet, so `sbx executor install wick` reports
> `no executor named "wick" in any repository` for now. You can install an executor from a directory meanwhile
> ([install guide](docs/guide/install.md#build-from-source)), or add another repository.

The installer puts `sbx` and `sbx-shim` in your OS's place for user programs (`~/.local/bin`, or
`%LOCALAPPDATA%\Programs\sbx` on Windows), never in the directory you run it from, after checking their SHA-256
(and their Sigstore signature when `cosign` is installed). Then you pick an executor.

### macOS

macOS 15 or later on Apple Silicon. No setup needed.

```sh
curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
sbx executor install wick        # Firecracker for Hypervisor.framework, with its adapter and guest kernel
sbx info                         # check it worked
```

### Linux

x86_64 or aarch64 with KVM (bare metal, or a VM with nested virtualization), and read-write access to `/dev/kvm`:

```sh
sudo usermod -aG kvm "$USER"     # if `ls -l /dev/kvm` shows you can't use it; then log out and back in
curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
sbx executor install wick        # Firecracker for KVM, with its adapter and guest kernel
sbx info                         # says what's missing if /dev/kvm isn't usable
```

The binaries are static, so any distribution works.

### Windows

Windows 10 or later on x86_64, in PowerShell. No administrator rights needed:

```powershell
irm https://toollyst.github.io/sandbox/install.ps1 | iex     # sbx.exe into %LOCALAPPDATA%\Programs\sbx
sbx info
```

`sbx` on Windows can `pull`, `prepare` and list images, but there is no tested executor for Windows yet, so it can't
run sandboxes.

Installer options (`SBX_VERSION`, `SBX_INSTALL_DIR`, `SBX_VERIFY`, the same for both installers), building from source, upgrading and
uninstalling are in the [install guide](docs/guide/install.md).

## Use it

```sh
sbx run --rm alpine echo hello                       # attached, removed afterwards
sbx run --rm -it alpine sh                           # interactive shell
sbx run -d --name dev alpine sleep 600               # detached
sbx exec -it dev sh
sbx cp dev:/etc/alpine-release .
sbx rm -f dev

sbx run --rm --network allow --allow pypi.org:443 --allow files.pythonhosted.org:443 \
    python:3.12-alpine pip install requests          # only these two hosts are reachable

sbx tui                                              # sandboxes, images, events and executors on one screen
```

Give an AI agent its own machine through MCP:

```sh
claude mcp add sbx -- sbx mcp                        # Claude Code; other clients run `sbx mcp` the same way
```

| Guide | |
| --- | --- |
| [Use it like Docker](docs/guide/usage.md) | Lifecycle, files, the network allow list, images, `--format json`, `--jq`, `events`, exit codes |
| [AI agents (MCP)](docs/guide/mcp.md) | Tools, limits, and what an agent can and can't reach |
| [Executors](docs/guide/executors.md) | Versions the nvm way, adapters and VMM packages, bringing your own VMM, signed repositories |
| [Install](docs/guide/install.md) | Per OS, installer options, from source, upgrade and uninstall |

## Platforms

Any executor that speaks the protocol runs anywhere it can start a VM. These are the ones tested here:

| Host | Tested VMM | Status |
| --- | --- | --- |
| macOS 15+, Apple Silicon | Firecracker on Hypervisor.framework ([wick](https://github.com/toollyst/wick)) under the `firecracker` adapter | Works. Real-VM checks run on a Mac, since CI's macOS runners can't start guests |
| Linux x86_64 | Upstream Firecracker on KVM under the `firecracker` adapter | Works. Real VMs boot in CI on every push and before every release |
| Linux aarch64 | Upstream Firecracker on KVM under the `firecracker` adapter | Released and unit-tested on arm; CI's arm runners have no KVM, so there is no real-VM run in CI yet |
| Windows 10+, x86_64 | – | Installs (`install.ps1`); `pull`, `prepare` and `images` work. No tested executor yet, so no sandboxes |

## How it works

`sbx` pulls the image and turns it into a read-only EROFS disk in pure Rust (no `mkfs`, no loop device, no `sudo`),
then asks the executor for a VM. Inside, a small init embedded in `sbx` mounts the image under a per-sandbox disk and
serves exec, file copy and the network allow list over vsock. The [developer docs](docs/developers/) follow one
`sbx run` from the CLI to the guest and back and map every crate.

## Development

```sh
cargo test --workspace --all-features   # unit, conformance, CLI end to end (no VM)
cargo xtask check-deps                  # dependency direction between and inside crates
cargo xtask dist && cargo xtask smoke   # checks on real VMs
```

Start with the [developer docs](docs/developers/). The design notes and [roadmap](docs/roadmap.md) in `docs/` and
the per-crate rules in each crate's docs are written in Korean.

## Not Docker Sandboxes

Docker's [Docker Sandboxes](https://docs.docker.com/ai/sandboxes/) also ships a CLI named `sbx`, and the two are
unrelated. Docker Sandboxes is a Docker product for running coding agents in microVMs, used with a Docker account.
This `sbx` is an independent open-source project: a Docker-style CLI and MCP server that runs any OCI image in its own
microVM, with no account, daemon or root, and with VMMs plugged in through an open executor protocol.

## Contributing and security

Bug reports, benchmark numbers from your machine and executors for other VMMs are all welcome: see
[CONTRIBUTING.md](CONTRIBUTING.md). Please report vulnerabilities privately, as [SECURITY.md](SECURITY.md) describes.
It also spells out the threat model: what `sbx` defends against, and what it doesn't.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
