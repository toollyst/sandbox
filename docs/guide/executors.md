# Executors

`sbx` doesn't hard-code a VMM. It hands each sandbox to an **executor**: a program that speaks the small, documented
[executor protocol](../../crates/spec/executor-proto/src/lib.rs) and starts the VM. Executors come from
[repositories](#repositories) or from a directory of your own, and install and switch the way Node or Go versions do
with nvm or gvm. `sbx` alone has none; the first one you install becomes the default.

- [Kinds of executor](#kinds-of-executor)
- [Install and switch versions](#install-and-switch-versions)
- [Bring your own VMM](#bring-your-own-vmm)
- [Repositories](#repositories)
- [Publish your own repository](#publish-your-own-repository)

## Kinds of executor

An executor is one of:

- **A VMM package**: just a VMM binary, run by an **adapter**. The adapter is the program that speaks the executor
  protocol, and it ships the default guest kernel. `wick` is a VMM package for the `firecracker` adapter.
- **Self-contained**: one program that speaks the protocol and brings its own VMM.

`sbx executor install` installs a VMM package's adapter first if it is missing. One adapter serves every VMM package
that names it, and a VMM package always runs under the highest installed version of its adapter, so installing a
newer adapter changes what runs every VMM package that names it. `sbx executor adapter ls|install|search|rm` manages
adapters directly.

```text
sbx ── executor protocol ──▶ adapter (firecracker) ──▶ VMM package (wick: firecracker) ──▶ KVM / Hypervisor.framework
```

## Install and switch versions

```sh
sbx executor search                        # newest patch of each main version for this host (alias: ls-remote)
sbx executor search --all                  # every patch
sbx executor install wick                  # the highest version; same as wick@latest
sbx executor install wick@1.18.0           # the newest patch of main version 1.18.0
sbx executor ls                            # installed (* marks the default), with where each came from (FROM)
sbx executor use wick@1.18.0               # the global default (the newest installed 1.18.0 patch)
echo wick@1.18.0 > .sbx-executor           # this folder and everything below it
sbx executor current                       # what a new sandbox would use here, and why
sbx executor which wick                    # the program that runs it (for a VMM package, its adapter)
sbx run --executor wick@1.18.0+wick.2 --rm alpine true   # just this once (or SBX_EXECUTOR)
sbx executor rm wick@1.18.0+wick.1         # refused while a sandbox is pinned to it, unless -f
sbx executor prune -n                      # what prune would remove: old versions nothing uses
```

A new sandbox uses the first of: `--executor` (an installed `NAME[@VERSION]` or a path to a program),
`.sbx-executor` in the current folder or the nearest one above it, the default, and `sbx-executor` on `PATH`. A
sandbox remembers the installed executor it was created with, so changing the default never changes what `start` or
`restart` boots. The rules are in [`choose.rs`](../../crates/app/cli/src/choose.rs).

A version has a **main version** and a **patch**, like Python's 3.12 and 3.12.4: the main version is everything
before the first `-` or `+`. Name the main version and you get its newest patch; name the full version to pin one
exactly. The rule is the same for every executor; with `wick`, a fix of its 1.18.0 build is `1.18.0+wick.1`:

| You write | You get |
| --- | --- |
| `wick`, `wick@latest` | the highest version |
| `wick@1.18.0` | the newest of `1.18.0`, `1.18.0+wick.1`, `1.18.0+wick.2`, … |
| `wick@1.18.0+wick.1` | exactly that patch |

Names and versions are lowercase, so on macOS and Windows a different spelling can't land in the same directory.

## Bring your own VMM

The `firecracker` adapter drives Firecracker through its public interface (a config file and vsock), so it runs
upstream Firecracker on Linux, [wick](https://github.com/toollyst/wick) on macOS, and your own Firecracker build too.
A different VMM needs an adapter of its own: any program that speaks the protocol, which `sbx` treats exactly like
ours. Register a build of your own as a VMM package, or install a self-contained executor:

```sh
sbx executor install --from ./my-firecracker --adapter firecracker myfc@1.19.0-dev   # a directory with firecracker
sbx executor install --from ./my-executor myvmm@0.1.0                               # sbx-executor at the top
```

What the protocol asks of an executor (`capabilities` prints one JSON line within 30 seconds; `run` reads a `Launch`
request on stdin and reports `Started` and `Exited`), and how to test one, is in the
[developer docs](../developers/how-it-works.md).

## Repositories

Executors and adapters come from **static repositories** that follow an
[open spec](../../crates/spec/executor-repo/src/lib.rs): an index plus `.tar.gz` packages, with OCI platform names
and SHA-256 and size checks. One comes preconfigured as `official`
(<https://toollyst.github.io/sandbox-executors/>, published from
[toollyst/sandbox-executors](https://github.com/toollyst/sandbox-executors)). It is a repository like any other: you
can remove it or put yours before it.

```sh
sbx executor repo add mine https://my.example.com/executors --key RWQ...   # its minisign public key
sbx executor repo ls                # in priority order, with SIGNED; `official` comes first
sbx executor install --repo mine myfc        # from that repository only
sbx executor repo rm official
```

`sbx` only reads repositories, and once an executor is installed it runs without them.

**Signed repositories.** A signed repository's index carries a `serial` and an `expires` inside its minisign
signature. `sbx` refuses an index whose signature doesn't match, that has expired (a frozen mirror), or whose serial
is lower than one it has already seen from that repository (a rollback). A repository added with `--unsigned`
relies on TLS alone, and `sbx` says so each time it reads one. A remote index can only point at `https` archives, and
every redirect is checked.

**A name belongs to one repository.** `install NAME` takes NAME from the first repository in `repo ls` order that
publishes it, even if a later one has a higher version, and once installed, a name only comes from where it came from
(`sbx executor ls` shows it under FROM). So adding a repository can't take over an executor you already use. If a
repository earlier in the list can't be read (down, tampered with, rolled back), `install` won't take a name from one
after it, since the unreadable one may own that name; fix it, or say where with `--repo`. `local` is reserved: it
marks executors you installed with `--from`.

Adapter names belong to repositories the same way. If a VMM package's adapter isn't installed yet, `install` takes it
from the first repository that publishes that adapter, and only from there; an adapter already installed from
another repository isn't used for it. So a repository of VMM packages for `firecracker` doesn't need to carry the
adapter, and a repository added later can't bring in its own `firecracker` that would then run everyone's VMM
packages.

**Yanked versions and advisories.** A repository can yank a version it published (a broken or vulnerable build) and
attach a one-line advisory to any version. `install NAME` and `install NAME@1.18.0` skip yanked versions; naming the
exact version (`NAME@1.18.0+wick.2`) still installs it, with a warning, so a pinned project keeps working. What you
already installed keeps running. `sbx executor search` marks yanked versions, and `sbx executor ls`, `adapter ls` and
`sbx tui` warn about installed ones (as of the last `search` or `install`; `ls` itself never goes online). In JSON
they are the `yanked` and `advisory` fields.

## Publish your own repository

A repository is a directory you can serve from GitHub Pages, S3 or a plain web server. The tools are in this
repository's `xtask`:

```sh
cargo xtask repo-keygen --out ./mine         # mine.pub to hand out, mine.key to keep secret
cargo xtask repo-add --repo ./executors --schema 2 --vmm-for firecracker --arch arm64 \
    --bundle ./my-firecracker --name myfc --version 1.19.0
cargo xtask repo-add --repo ./executors --schema 2 --bundle ./my-executor --name myvmm --version 0.1.0
cargo xtask repo-sign --repo ./executors --key ./mine.key     # again after every change, and before it expires
cargo xtask repo-yank --repo ./executors --schema 2 --name myvmm --version 0.1.0 \
    --advisory "CVE-2026-0001: upgrade to 0.1.1"                # then repo-sign again; --undo takes it back
```

Then upload the directory as is. The official repository is built exactly this way; its workflow in
[toollyst/sandbox-executors](https://github.com/toollyst/sandbox-executors) is a complete example.
