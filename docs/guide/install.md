# Install

`sbx` is two files, `sbx` and `sbx-shim`. They run nothing by themselves: sandboxes boot through an **executor**, a
separate program that speaks the open [executor protocol](executors.md). The installer installs both: `sbx`, then the
executor its version pins. Installed `sbx` some other way? The first command that needs a VM fetches the default
executor from the signed official repository itself ([not yet in this release](#the-executor)). Nothing runs as root
and nothing runs in the background.

- [macOS](#macos)
- [Linux](#linux)
- [Windows](#windows)
- [The executor](#the-executor)
- [Installer options](#installer-options)
- [Installer versions](#installer-versions)
- [Build from source](#build-from-source)
- [Upgrade and uninstall](#upgrade-and-uninstall)

## macOS

You need macOS 15 or later on Apple Silicon. Nothing else: no admin rights, no kernel extension, no setup.

```sh
curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
sbx info                         # host, virtualization (Hypervisor.framework), executor
sbx run --rm alpine echo hello
```

The default executor today is `wick`, a Firecracker build that also runs on Hypervisor.framework
([toollyst/wick](https://github.com/toollyst/wick)), with its `firecracker` adapter and guest kernel.
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

## The executor

`sbx` needs one executor installed; which one is up to you. When it has none:

- **The installer** installs the executor its version pins (below), from the official repository, with
  `sbx executor install`. It leaves alone an executor you already use (a different default, or `SBX_EXECUTOR`), and
  `SBX_EXECUTOR=none` skips this step. If the step fails (offline, say), `sbx` is still installed and the next item
  takes over.
- **The first `sbx run`, `create`, `start` or `build`** installs the newest default executor from the official
  repository and carries on. It prints one line on stderr saying what it installs and from where, and asks nothing,
  so it works the same in scripts, CI and `sbx mcp`: it only takes a package the repository signed for, checked the
  same way as `sbx executor install`. Which executor is the default is said by the repository's index, not built into
  `sbx`. Two commands starting at once install it once. When the repository can't be reached, the command fails with
  the reason and the manual ways (`sbx executor install`, `--from DIR`).
- It never happens when any executor is installed, when you give `--executor`, or with `SBX_NO_AUTO_EXECUTOR=1` or
  `SBX_EXECUTOR=none`. `sbx executor current` says whether it would.

**In this release** neither happens yet: `sbx` doesn't carry the official repository's public key, so it treats
`official` as unsigned and never installs from it on its own (`sbx executor current` and `sbx info` say so), and the
installer version pins no executor. Install one yourself: `sbx executor search` lists what the configured
repositories offer, `sbx executor install NAME` installs one (from an unsigned repository, with a warning that only
TLS protects it), and `--from DIR` installs one you built ([Build from source](#build-from-source)).

After that, executors are yours to manage: `sbx upgrade` upgrades only `sbx`, and `sbx executor install` the executor
([executors](executors.md)).

## Installer options

Both installers ([`install.sh`](../../install.sh) for macOS and Linux, [`install.ps1`](../../install.ps1) for Windows)
do the same: download the release archive for your OS and architecture, check its SHA-256, check its Sigstore
signature when `cosign` is installed, and copy `sbx` and `sbx-shim` into your OS's place for user programs. They
never write to the directory you run them from. `install.sh` then installs the executor (above); Windows has no
executor yet.

| Variable | Default | What it does |
| --- | --- | --- |
| `SBX_VERSION` | the release this installer pins | Install this tag instead, e.g. `v0.2.0`. The signature must come from exactly the tag installed |
| `SBX_INSTALL_DIR` | `~/.local/bin`; Windows: `%LOCALAPPDATA%\Programs\sbx` | Where `sbx` and `sbx-shim` go |
| `SBX_VERIFY` | `auto` | `auto` checks the signature when `cosign` is on `PATH`, and then a missing signature is an error; `require` also fails without `cosign`; `off` checks only the SHA-256 |
| `SBX_EXECUTOR` | unset | `none`: install no executor (and `sbx` won't fetch one by itself while it is set). Any other value is an executor you chose, so none is installed |

```sh
curl -fsSL https://toollyst.github.io/sandbox/install.sh | SBX_VERSION=v0.2.0 SBX_VERIFY=require sh
```

```powershell
$env:SBX_VERSION = 'v0.2.0'; $env:SBX_VERIFY = 'require'; irm https://toollyst.github.io/sandbox/install.ps1 | iex
```

To check a release by hand instead, see [SECURITY.md](../../SECURITY.md#verifying-what-you-install).

## Installer versions

The installer has its own version number, separate from `sbx`'s and the executor's; the three move independently.
Each installer version names the `sbx` release and the executor (and adapter) version it installs, tested together
before it was published. Every version stays published, unchanged, as `install-<version>.sh` and
`install-<version>.ps1`, and `install.sh` and `install.ps1` are copies of the newest. So a pinned installer always
installs the same things:

```sh
curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh -s -- --version   # what the newest one installs
curl -fsSL https://toollyst.github.io/sandbox/install-0.0.1.sh | sh             # exactly that version
```

The first line it prints when it runs says the same. The pairing is a recommendation, not a lock: once installed,
nothing checks that `sbx` and the executor still match.

## Build from source

You need Rust (stable) and the `*-unknown-linux-musl` target for your machine's architecture: the guest init is
built for it and embedded in `sbx`.

```sh
git clone https://github.com/toollyst/sandbox && cd sandbox
rustup target add "$(uname -m | sed s/arm64/aarch64/)-unknown-linux-musl"   # Windows: x86_64-unknown-linux-musl
cargo xtask install               # builds sbx and sbx-shim and installs them where the installers do
sbx run --rm alpine echo hello    # needs an executor (see "The executor" above)
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
