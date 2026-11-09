# sbx

**Docker-style sandboxes, each in its own microVM.**

`sbx` runs any OCI image (Docker Hub, GHCR, your own registry) inside a hardware-isolated
[Firecracker](https://firecracker-microvm.github.io/) microVM with its own Linux kernel. You use it the way you already
use `docker`, and a full boot → run → shutdown → cleanup cycle takes about **120 ms**.

<p align="center">
  <img src="assets/demo.svg" width="876" alt="Terminal recording: sbx run prints Linux 6.1.155; time shows how long one run takes; the sandbox has no network device; with --network allow only example.com is reachable and github.com gets 403; a detached sandbox is exec'd into, listed with ps and removed; a Python image runs.">
</p>

<sub>A real recording (`cargo xtask demo`, macOS on Apple Silicon): every command ran for real, and the output appears
when it actually arrived. Only the typing is simulated.</sub>

Built for running code you don't trust: AI agents (there is a built-in [MCP server](#use-it-from-ai-agents-mcp)),
user-submitted code, CI jobs, plugins.

> **Status: pre-release.** It works end to end on Apple Silicon Macs and on Linux x86_64 with KVM (real VMs boot in CI
> on every push). Prebuilt binaries and executor bundles come with the first release. See [Status](#status).

## Why

- **A kernel boundary, not just namespaces.** Containers share the host kernel. Each `sbx` sandbox gets its own
  kernel behind hardware virtualization (KVM on Linux, Hypervisor.framework on macOS).
- **Docker muscle memory.** `run`, `exec`, `cp`, `logs`, `ps`, `stop`, `kill`, `wait`, `attach`, `rm`, `-i`, `-t`, `-d`,
  `-e`, `-w`, `-u`, `--rm`, `--name`, ID prefixes, and exit codes 125/126/127 work the way you expect. Where `sbx` differs, the
  difference is deliberate and [written down](crates/sandbox-cli/src/behavior.rs).
- **Locked down by default.** There is no network device at all. Outbound access is opt-in, per `host:port`, enforced on
  the host, with names that resolve to private addresses refused. The image's root filesystem is read-only, and every
  sandbox writes to its own disk. The [threat model](SECURITY.md) spells out what is and isn't covered.
- **Fast.** Images are converted once into an EROFS disk image. After that, starting a sandbox is just booting a VM.
- **No daemon, no root.** `sbx` is a single binary. A detached sandbox is owned by a small per-sandbox `sbx-shim`
  process. On macOS no privileges are needed; on Linux you need access to `/dev/kvm`.
- **Pure Rust image pipeline.** Pulling, layer merging (gzip and zstd, whiteouts, hard links), and EROFS writing are all
  done in Rust. There are no `mkfs`, loop devices, or `sudo`, and it works the same on macOS and Linux.
- **Pluggable VMMs.** `sbx` talks to VMMs through a small, documented [executor protocol](crates/executor-proto/src/lib.rs).
  Executors install and switch like a language version manager does (`sbx executor install/use`), from any static
  repository (an [open spec](crates/executor-repo/src/lib.rs) that works with GitHub Pages, S3, or a plain directory).

## Numbers

Measured with `cargo xtask bench` on an Apple Silicon Mac (macOS, Hypervisor.framework), 1 vCPU / 512 MiB per sandbox:

| What | p50 | p90 |
| --- | --- | --- |
| `sbx run --rm alpine:3.20 true` (boot, run, shut down, delete) | **121 ms** | 139 ms |
| `sbx exec <id> true` (in a running sandbox) | **8 ms** | 10 ms |
| 10 × `sbx run --rm alpine:3.20 true` at once, until all are done | **0.50 s** | |

Run the benchmark on your own machine: `cargo xtask bench` (see [`xtask/src/bench.rs`](xtask/src/bench.rs)).

## Quick start

### Install

```sh
curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
```

This puts `sbx` and `sbx-shim` in `~/.local/bin` (checked against their SHA-256) and installs the `firecracker`
executor (VMM, adapter, and guest kernel) from the official executor repository. It needs Linux x86_64/aarch64 with
`/dev/kvm`, or an Apple Silicon Mac. Nothing runs as root. Set `SBX_VERSION`, `SBX_INSTALL_DIR`, or
`SBX_NO_EXECUTOR=1` to change what it does; the script is [short](install.sh).

> The installer works from the first release on. Until then, build from source.

Then skip to [Run things](#run-things).

### Or build from source

You need Rust (stable) and, for the guest init, the `*-unknown-linux-musl` target of your machine's architecture.

```sh
git clone https://github.com/toollyst/sandbox && cd sandbox
rustup target add "$(uname -m | sed s/arm64/aarch64/)-unknown-linux-musl"
cargo xtask dist        # builds target/release/sbx and target/release/sbx-shim (guest init embedded)
export PATH="$PWD/target/release:$PATH"
```

An executor bundle holds three things: the adapter (`sbx-executor`), the VMM (`firecracker`), and a guest kernel.

```sh
# from the official repository (built in), or any other static one (see "Executors")
sbx executor install firecracker

# or from a local bundle directory or .tar.gz
sbx executor install --from ./firecracker-bundle firecracker@0.1.0
sbx executor ls
```

Until prebuilt bundles are published, build one yourself (this is exactly what CI does on Linux):

```sh
arch=$(uname -m | sed s/arm64/aarch64/)
cargo build --release -p executor-firecracker
cargo xtask kernel --arch "$arch"          # Linux 6.1 + Firecracker's CI config + sbx's needs, built in Docker
mkdir bundle
cp target/release/sbx-executor-firecracker bundle/sbx-executor
cp "target/kernel/$arch/kernel" bundle/kernel
cp /path/to/firecracker bundle/firecracker   # an upstream Firecracker release on Linux/KVM
sbx executor install --from bundle firecracker@local
cargo xtask smoke                          # 17 checks on a real VM
```

### Run things

```sh
sbx run --rm alpine echo hello                       # attached, removed afterwards
sbx run --rm -it alpine sh                           # interactive shell with a TTY
sbx run --rm --cpus 2 -m 1g python:3.12-alpine python -c 'print(42)'

id=$(sbx run -d alpine sleep 600)                    # detached
sbx ps
sbx exec "${id:0:6}" cat /etc/alpine-release           # any unique ID prefix works
sbx logs -f "$id"
sbx stop "$id" && sbx rm "$id"

sbx run -d --name dev alpine sleep 600
sbx exec dev uname -r                                # names work wherever IDs do
sbx rm -f dev
```

## A tour

**Files.** Copy a project in at start, and copy results out:

```sh
sbx run --rm --mount type=copy,source=./proj,target=/work -w /work alpine sh -c 'ls; cat main.py'
sbx cp "$id":/out/result.json ./result.json
```

Live sharing (`-v ./dir:/work[:ro]`) works with executors that support virtio-fs. The Firecracker executor doesn't yet,
and `sbx` says so instead of silently copying.

**Network.** The VM never gets a network interface. With `--network allow`, an HTTP proxy inside the guest hands each
connection to the host over vsock, and the host checks it against your list:

```sh
sbx run --rm --network allow \
    --allow pypi.org:443 --allow files.pythonhosted.org:443 \
    python:3.12-alpine pip install requests
```

Rules can be `host:port`, `*.example.com:443`, `10.0.0.5:*` or `[::1]:80`. Names that resolve to loopback or private
addresses are refused unless the rule names that IP (or `localhost`) explicitly.

**Lifecycle.** `create`, `start [-a] [-i]`, `stop [-s SIG] [-t SECS]` (the image's `StopSignal`, then power off),
`restart`, `kill [-s SIG]`, `wait`, `attach` (detach with Ctrl-P Ctrl-Q), and `rm [-f]`. A stopped sandbox keeps its
writable disk, so `start` picks up where it left off.

**Images.** `pull`, `prepare` (build the root filesystem disk image ahead of time), `images`, `rmi [-f]`. `run` pulls
and prepares on first use (`--pull=missing|always|never`).

**Scripting.** `--format json` on `ps`, `inspect`, `images`, `executor ls`, and `executor search` gives machine-readable
output.

## Use it from AI agents (MCP)

`sbx mcp` serves sandboxes over the [Model Context Protocol](https://modelcontextprotocol.io) on stdio, so any MCP
client can run code in a microVM instead of on your machine:

```sh
claude mcp add sbx -- sbx mcp          # Claude Code
```

```json
{ "mcpServers": { "sbx": { "command": "sbx", "args": ["mcp"] } } }
```

| Tool | What it does |
| --- | --- |
| `run` | Run one command in a fresh sandbox, return exit code, stdout and stderr, then delete it |
| `create` | Start a long-lived sandbox, optionally named (it survives the MCP server) |
| `exec` | Run a command in a running sandbox |
| `write_file` / `read_file` | Move files in and out (text or base64; parent directories are created) |
| `list` / `remove` | See and clean up sandboxes |

The MCP layer adds no policy. Tool arguments go through the same parser and rules as the CLI. The network stays off
unless a call passes `allow: ["host:port", ...]`, host folders are never mounted, and arguments can't be turned into
other `sbx` options. Commands have a timeout (default 300 s); after it, `run` powers the VM off and `exec` kills the
process.

## How it works

```text
 sbx (CLI) ──────────────────────────────────────────────────────────────────────────────┐
   │  manage core: pull OCI image → merge layers → write EROFS disk image (pure Rust, cached) │
   │  run core:    sandbox records, lifecycle, guest protocol, egress allow list            │
   ▼                                                                                        │
 executor protocol (line-delimited JSON over stdio)                                        │
   ▼                                                                                        │
 sbx-executor-firecracker ── firecracker --no-api --config-file ── KVM / HVF               │
                                         │                                                  │
                         ┌───────────────┴────────────────────────────┐                     │
                         │ microVM                                    │                     │
                         │  kernel (bundle default, or the image's)   │                     │
                         │  guest init (PID 1, Rust, static)          │◄── vsock ───────────┘
                         │  /  = overlay( EROFS image, read-only      │
                         │              + per-sandbox ext2 disk )     │
                         └────────────────────────────────────────────┘
```

- The **guest init** is embedded in the `sbx` binary and boots as the initramfs. It mounts the image read-only, layers
  the sandbox's own disk on top, and serves exec, file-copy and egress requests over vsock.
- Your **images are never modified**. The kernel comes from the executor bundle (one per architecture, shared by every
  image). An image can also carry its own kernel: add the label `io.github.toollyst.sandbox.kernel=/path/in/image` and
  `sbx` boots that kernel instead.
- Crates have **one-way dependencies**, enforced by `cargo xtask check-deps`: model → protocols → cores → CLI. The cores
  never pick implementations, and only binaries do.

## Executors

An executor is any program that speaks the [executor protocol](crates/executor-proto/src/lib.rs): `capabilities`
prints a JSON line, and `run` takes a `Launch` request on stdin and reports `Started` and `Exited`. The VMM and kernel are
the executor's business. This repo ships an adapter for Firecracker's public interface (config file and vsock), so it
works with upstream Firecracker on Linux and with any build that implements the same interface.

Executor **repositories** are static: one `index.json` plus `.tar.gz` bundles, with OCI platform names, SHA-256 and
size checks, and relative URLs. Host one anywhere, like an apt repo:

```sh
sbx executor repo add mine https://my.example.com/executors
sbx executor search
sbx executor install firecracker@0.1.0
sbx executor use firecracker@0.1.0
```

To publish your own, add bundles to a directory and upload it as is:

```sh
cargo xtask repo-add --repo ./executors --bundle ./bundle --name firecracker --version 0.1.0
```

## Platforms

| Host | VMM | Status |
| --- | --- | --- |
| macOS, Apple Silicon | Firecracker ported to Hypervisor.framework ([toollyst/firecracker](https://github.com/toollyst/firecracker)) | works end to end |
| Linux x86_64 | upstream Firecracker on KVM | works end to end (real-VM checks in CI) |
| Linux aarch64 | upstream Firecracker on KVM | test suite passes; real-VM checks wait for an arm runner with KVM |
| Windows | – | not yet |

## Use it as a library

The CLI is just one presentation of the cores. `sandbox-manage` (images and cache) and `sandbox-runtime` (sandbox
lifecycle) are plain Rust crates with no CLI assumptions, and they can share or separate state directories. Bindings
for other languages are planned.

## Development

```sh
cargo test --workspace --all-features     # unit, protocol conformance, and CLI end-to-end tests (fake executor, no VM)
cargo xtask check-deps                    # dependency direction between and inside crates
cargo xtask dist && cargo xtask smoke     # real-VM checks with the installed executor
cargo xtask bench                         # real numbers on your machine
cargo xtask demo                          # re-record assets/demo.svg (real commands, real timing)
cargo xtask kernel --arch x86_64          # the default guest kernel (Docker; cross-compiles if needed)
```

Design notes and the roadmap are in [`docs/`](docs/), and per-crate rules are in each crate's docs. Both are written in
Korean.

## Status

Pre-release. Done: the full Docker-style lifecycle, images, the egress allow list, the executor protocol v2 and
repositories, image-provided kernels, the macOS/HVF end to end, and the release pipeline (static Linux binaries,
executor bundles with a CI-built kernel, the official repository, and the installer). Next: the first release and
a Linux aarch64 real-VM run. See the [roadmap](docs/roadmap.md).

## Not Docker Sandboxes

Docker's [Docker Sandboxes](https://docs.docker.com/ai/sandboxes/) also ships a CLI named `sbx`. The two are unrelated.
Docker Sandboxes is a Docker product for running coding agents in microVMs, used with a Docker account. This `sbx` is
an independent open-source project: a Docker-compatible CLI (and MCP server) that runs any OCI image in its own
microVM, with no account, daemon or root, and VMMs plugged in through an open executor protocol.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report vulnerabilities privately, as described in
[SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
