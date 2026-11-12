# Install

`sbx` is two files, `sbx` and `sbx-shim`. They run nothing by themselves: sandboxes boot through an **executor**, which
you install next. Nothing runs as root and nothing runs in the background.

- [macOS](#macos)
- [Linux](#linux)
- [Windows](#windows)
- [Installer options](#installer-options)
- [Build from source](#build-from-source)
- [Upgrade and uninstall](#upgrade-and-uninstall)

## macOS

You need macOS 15 or later on Apple Silicon. Nothing else: no admin rights, no kernel extension, no setup.

```sh
curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
sbx executor install wick        # the Firecracker VMM for macOS, with its adapter and guest kernel
sbx info                         # host, virtualization (Hypervisor.framework), executor
sbx run --rm alpine echo hello
```

`wick` is a Firecracker build that runs on Hypervisor.framework ([toollyst/wick](https://github.com/toollyst/wick)).
How wick confines itself on macOS is wick's part; [SECURITY.md](../../SECURITY.md) says what that means for you.
Intel Macs aren't supported.

## Linux

You need x86_64 or aarch64 with KVM: bare metal, or a VM with nested virtualization. Your user needs read and write
access to `/dev/kvm`, usually through the `kvm` group:

```sh
ls -l /dev/kvm                           # missing: turn on virtualization in the firmware, or use nested virtualization
sudo usermod -aG kvm "$USER"             # then log out and back in
```

Then:

```sh
curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
sbx executor install wick        # Firecracker for KVM, with the firecracker adapter and guest kernel
sbx info                         # says what's missing if /dev/kvm isn't usable
sbx run --rm alpine echo hello
```

The binaries are static (musl), so any distribution works. `~/.local/bin` must be on your `PATH`; the installer says
so if it isn't.

## Windows

You need Windows 10 or later on x86_64. In PowerShell (Windows PowerShell 5.1 or PowerShell 7), with no administrator
rights:

```powershell
irm https://toollyst.github.io/sandbox/install.ps1 | iex
sbx info
sbx pull alpine
```

The installer puts `sbx.exe` and `sbx-shim.exe` in `%LOCALAPPDATA%\Programs\sbx` and prints the command that adds it
to your user `PATH`; it doesn't change `PATH` itself. On Windows, `sbx` can `pull`, `prepare` and list images, but
there is no tested executor for Windows yet, so it can't run sandboxes. [Read the installer](../../install.ps1)
before you run it.

## Installer options

Both installers ([`install.sh`](../../install.sh) for macOS and Linux, [`install.ps1`](../../install.ps1) for Windows)
do the same: download the release archive for your OS and architecture, check its SHA-256, check its Sigstore
signature when `cosign` is installed, and copy `sbx` and `sbx-shim` into your OS's place for user programs. They
never write to the directory you run them from or anywhere else, and don't install an executor.

| Variable | Default | What it does |
| --- | --- | --- |
| `SBX_VERSION` | the latest release | Install this tag, e.g. `v0.2.0`. The signature must then come from exactly that tag |
| `SBX_INSTALL_DIR` | `~/.local/bin`; Windows: `%LOCALAPPDATA%\Programs\sbx` | Where `sbx` and `sbx-shim` go |
| `SBX_VERIFY` | `auto` | `auto` checks the signature when `cosign` is on `PATH`, and then a missing signature is an error; `require` also fails without `cosign`; `off` checks only the SHA-256 |

```sh
curl -fsSL https://toollyst.github.io/sandbox/install.sh | SBX_VERSION=v0.2.0 SBX_VERIFY=require sh
```

```powershell
$env:SBX_VERSION = 'v0.2.0'; $env:SBX_VERIFY = 'require'; irm https://toollyst.github.io/sandbox/install.ps1 | iex
```

To check a release by hand instead, see [SECURITY.md](../../SECURITY.md#verifying-what-you-install).

## Build from source

You need Rust (stable) and the `*-unknown-linux-musl` target for your machine's architecture: the guest init is
built for it and embedded in `sbx`.

```sh
git clone https://github.com/toollyst/sandbox && cd sandbox
rustup target add "$(uname -m | sed s/arm64/aarch64/)-unknown-linux-musl"   # Windows: x86_64-unknown-linux-musl
cargo xtask install               # builds sbx and sbx-shim and installs them where the installers do
sbx executor install wick
```

`cargo xtask install` puts them in the same place as the installers (`~/.local/bin`, or `%LOCALAPPDATA%\Programs\sbx`
on Windows; `--dir DIR` to choose), so you don't run `sbx` out of the build directory and a rebuild doesn't change
what you installed. `cargo xtask dist` only builds them, into `target/release/`.

To build the executor too, the adapter is a directory with `sbx-executor` (built from `crates/executor/firecracker`)
and `kernel` (`cargo xtask kernel --arch <x86_64|aarch64>`, built in Docker), and the VMM package is a directory with
`firecracker`. Install both from those directories, then check them on real VMs:

```sh
sbx executor adapter install --from ./adapter firecracker@local
sbx executor install --from ./vmm --adapter firecracker wick@local
cargo xtask smoke
```

The official repository builds them the same way, in
[toollyst/sandbox-executors](https://github.com/toollyst/sandbox-executors).

## Shell completion

```sh
sbx completion zsh --help         # how to load it; bash, fish and PowerShell work the same way
```

Completion offers subcommands and flags, and also sandbox names and IDs and image references from your machine.

## Upgrade and uninstall

`sbx upgrade` replaces `sbx` and `sbx-shim` with the newest release, after checking its signature with the key built
into `sbx` (and its Sigstore signature when `cosign` is installed). Sandboxes, images and executors are kept, and
running sandboxes keep running. `sbx upgrade --check` only looks; `--version 0.2.2` picks a release. `sbx` also tells
you about a newer release once a day after a command; `SBX_NO_UPDATE_CHECK=1` turns that off (it never runs in CI).

Running the installer again works too, and is the way up from 0.2.1, which has no `sbx upgrade`. If a package
manager installed `sbx`, upgrade with that manager.

What changed in each release is in the [CHANGELOG](../../CHANGELOG.md). Releases before 0.2 are withdrawn and not
supported; if you have one, remove it and its data (below) and install again.

To uninstall, remove the binaries, then the data (sandboxes, executors, settings) and the cache (images):

| | Binaries | Data | Cache |
| --- | --- | --- | --- |
| macOS | `~/.local/bin/sbx`, `sbx-shim` | `~/Library/Application Support/sbx` | `~/Library/Caches/sbx` |
| Linux | `~/.local/bin/sbx`, `sbx-shim` | `${XDG_DATA_HOME:-~/.local/share}/sbx` | `${XDG_CACHE_HOME:-~/.cache}/sbx` |
| Windows | `%LOCALAPPDATA%\Programs\sbx` | `%LOCALAPPDATA%\sbx\data` | `%LOCALAPPDATA%\sbx\cache` |

`sbx rm -f $(sbx ps -aq)` first stops and removes any sandbox that is still running.
