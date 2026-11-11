# How it works

This page follows `sbx run --rm alpine echo hello` from the command line into a VM and back. Then it covers the
detached path, `exec` and `cp` from another process, the network, and the image pipeline. Each step names the code
that does it. The doc comment there is the precise rule.

## The pieces at run time

```text
 your terminal
     │
 sbx (sbx-cli) ──────────── cache_dir: blobs, images/<id>/rootfs.erofs, guest/…/initramfs,   (sbx-manage writes)
     │                                 events/images/
     │  sbx-runtime ─────── data_dir:  sandboxes/<id>/{spec.json,state.json,scratch.img,owner.lock,logs/},
     │                                 events/sandboxes/, executors/, adapters/ (the last two: sbx executor)
     │                      socket_dir: short paths for Unix sockets (macOS caps them at 104 bytes)
     │  stdin/stdout: executor protocol (line-delimited JSON)
     ▼
 sbx-executor (sbx-executor-firecracker) ── firecracker --no-api --config-file … ── KVM / HVF
                                                   │
                       control_socket ◄── vsock ──►│ microVM
                       (CONNECT 1024)              │  /init = sbx-guest (PID 1)
                                                   │  /dev/vda rootfs.erofs (read-only)
                                                   │  /dev/vdb scratch.img  (writable)
```

There are three directories, each passed in by the caller. The CLI's defaults are in
[`dirs.rs`](../../crates/app/cli/src/dirs.rs), and the `SBX_DATA_DIR`, `SBX_CACHE_DIR` and `SBX_SOCKET_DIR`
environment variables override them. Two processes given the same paths share state, and different paths keep them
apart.

Both `data_dir` and `cache_dir` hold a `layout-version` file (from 0.2.1): one decimal number, the on-disk layout
that wrote them. [`layout.rs`](../../crates/app/cli/src/layout.rs) checks it whenever `sbx` starts. A missing file
(a directory 0.2.0 wrote) is layout 1 and gets written; a higher number than this `sbx` knows is refused rather than
guessed at; a lower one is migrated in place. A change to the layout bumps `layout::CURRENT` and adds a step to
`layout::migrate`.

## `sbx run --rm alpine echo hello`

### 1. Parse and assemble (`sbx-cli`)

[`app.rs`](../../crates/app/cli/src/app.rs) `run` dispatches to `run_sandbox`. `create_with` turns the flags into a
`SandboxSpec`:

- `ProcessOverrides` holds the entrypoint, args, env (`--env-file` first, then `-e`), workdir and user.
- `Resources` holds the vCPUs, memory and scratch size.
- It also builds the mounts, labels and `NetworkPolicy`.

Parsing lives in [`parse.rs`](../../crates/app/cli/src/parse.rs). Defaults and Docker's rules live in
[`behavior.rs`](../../crates/app/cli/src/behavior.rs).

### 2. Pull and prepare if needed (`sbx-manage`)

`behavior::create_steps` decides from `--pull` and the cache contents whether to `pull`, `prepare`, both, or
neither. The CLI then calls the management core:

- **`pull_with`**: [`sbx-oci`](../../crates/adapter/oci) resolves `alpine` to
  `docker.io/library/alpine:latest`, fetches the index and picks `linux/<host arch>`, checks every document's
  sha256, and lists the layers. `sbx-manage` downloads only the missing layers. It checks each digest while writing
  and reports progress (the CLI draws the bar). The image record and reference are written last.
- **`prepare`**: [`sbx-rootfs`](../../crates/port/rootfs) merges the layers in memory (whiteouts, opaque
  directories, hard links) and writes one EROFS image to `images/<id>/rootfs.erofs`. Nothing is unpacked onto your
  disk, so the result is the same on macOS, Linux and Windows. If the image carries its own kernel (label
  `io.github.toollyst.sandbox.kernel`), it is extracted next to the rootfs.

### 3. Create the sandbox (`sbx-runtime`)

`Runtime::create`:

- **Validates the request against the executor's capabilities** ([`validate.rs`](../../crates/core/runtime/src/validate.rs)).
  For example, a live `-v` share with an executor that lacks `live_share` is an error, never a silent copy.
- **Merges the image config with the overrides** the way `docker run` does
  ([`process.rs`](../../crates/core/runtime/src/process.rs)).
- **Writes the record.** It builds `sandboxes/<id>/` in a temporary directory, with `spec.json`, `state.json`
  (`created`) and a sparse, zero-filled `scratch.img`, then renames it into place
  ([`records.rs`](../../crates/core/runtime/src/records.rs)).
- **Claims `--name`, if given**, as a file under `names/`.

`create` never pulls. If the image or rootfs is missing, it fails, and the CLI is the one that pulled in step 2.

### 4. Start: launch the VM (`sbx-runtime` → executor)

`Sandbox::start`:

1. **Takes ownership.** It holds `owner.lock` and a shared lease on the image in the cache, so `rmi` can't delete the
   rootfs under a running VM.
2. **Picks the boot files.** The guest init is the initramfs embedded in `sbx`, extracted once to
   `cache_dir/guest/sha256-<digest>/<arch>/initramfs`. The kernel is the executor's default, unless the image
   brought its own.
3. **Launches through the executor.** [`sbx-executor-process`](../../crates/adapter/executor-process) runs
   `sbx-executor run` and sends `Launch` (disks, initrd, kernel, vCPUs, memory, `control_socket`). Then it waits for
   `Started`. The executor's stderr goes to `logs/executor.log`.
4. **The executor starts the VMM.** [`sbx-executor-firecracker`](../../crates/executor/firecracker) writes a
   Firecracker config with vsock at `control_socket`, guest CID 3 and the kernel command line
   (`sbx.hostname=…`, `sbx.halt=reboot`), and runs `firecracker --no-api --config-file`. The config file sits next to
   `control_socket`, in the socket directory `sbx` made for this user, and is always written as a new `0600` file.

Whoever started the VM owns it. If that process dies, its pipe to the executor closes and the executor kills the VM
([executor protocol](../../crates/spec/executor-proto/src/lib.rs) rule 5). Every other process sees `owner.lock`
free and reports the sandbox as `stopped`. Nothing is ever orphaned.

### 5. Boot inside the VM (`sbx-guest`)

The kernel unpacks the initramfs and runs `/init` = `sbx-guest` as PID 1
([`boot.rs`](../../crates/app/guest/src/boot.rs)):

1. It mounts `/dev`, `/proc` and `/sys`, and reads the kernel command line.
2. It mounts `/dev/vda` (EROFS) read-only. It formats `/dev/vdb` on first boot (scratch is zero-filled, so "no
   filesystem" means "new") and reuses it after a restart.
3. It stacks overlay(rootfs ↓, scratch ↑), mounts `/proc`, `/sys`, `/dev`, `/dev/pts` and `/dev/shm` inside, moves
   it to `/`, and chroots. It brings up loopback and sets the hostname.
4. It re-executes itself as the **agent**, which listens on vsock port 1024 and serves each connection on its own
   thread. It also starts a loopback DNS responder that answers NXDOMAIN at once, so name lookups without network
   fail instantly instead of after a 5 s timeout.
5. PID 1 then only reaps orphans. When the agent exits, the VM powers off.

### 6. Run the command (host ↔ guest)

The runtime connects to `control_socket`, writes `CONNECT 1024`, and retries until the agent answers (`Started`
doesn't mean the guest is ready yet). The conversation follows [`sbx-guest-proto`](../../crates/spec/guest-proto),
with one connection per operation:

```text
host                                   guest (agent)
Hello{version}            ───────►
                          ◄───────     HelloAck
Exec{argv, env, cwd, user, tty…}  ──►  resolves the user in the image's /etc/passwd, spawns the process
                          ◄───────     Started{pid}
Stdin / Resize / Signal   ───────►
                          ◄───────     Stdout / Stderr …
                          ◄───────     Exited{code}
```

Before the main process starts, the runtime copies in `--mount type=copy` sources and asks the guest to mount live
shares. The CLI's [`attach.rs`](../../crates/app/cli/src/attach.rs) connects your terminal: raw mode with `-t`,
stdin with `-i`, window resizes, and signals.

### 7. Exit and clean up

When the main process exits, the runtime asks the guest to shut down (`Shutdown`: sync, then halt). It kills the VM if
it doesn't stop in time, and records `stopped` with the exit code. The exit code is the process's own, or 128 + signal
number. When the VM went away first, it is 137. The CLI exits with that code, or 125/126/127 when `sbx` itself, or
starting the command, failed. With `--rm` it then deletes the sandbox: it renames the directory away, removes it,
and releases the name.

## Detached: `sbx run -d`

`run -d` (and `start`) hand ownership to an [`sbx-shim`](../../crates/app/shim) process instead of keeping it in
`sbx`:

1. `sbx` creates the sandbox as above, then starts `sbx-shim start …` in a new process group and waits for one line:
   `ready` or `error <reason>`.
2. The shim runs `Sandbox::start` itself, so it holds `owner.lock` and the executor's pipe. It writes the main
   process's output to `logs/main.log`, each chunk with the time it arrived, so `sbx logs -t`, `--since` and `--until`
   work ([`log.rs`](../../crates/app/shim/src/log.rs) has the format). It listens on a socket for `sbx attach`.
3. `sbx` prints the ID and exits. The VM keeps running because its owner, the shim, is still alive.
4. When the main process exits, the shim records the result and exits too. SIGTERM or SIGINT to the shim stops the
   sandbox. If the shim is killed, the executor stops the VM, as in step 4 above.

## `exec`, `cp` and `stop` from another process

These don't go through the owner. Any process with the same `data_dir` reads `state.json` to find the
`control_socket` of the running VM, connects with `ConnectOnly`, and opens its own guest connection: `Exec`, `CopyIn`,
`CopyOut` or `Shutdown`. That is why `sbx exec` takes milliseconds and works the same for attached and detached
sandboxes. `cp` targets follow `docker cp` ([`cpplan.rs`](../../crates/core/runtime/src/cpplan.rs)).

## Events, JSON output and the TUI

There is no daemon to ask what changed, so the process that changes something writes it down.
`sbx-runtime` appends a line to `data_dir/events/sandboxes` whenever it records a sandbox as created, running,
stopped or removed, and `sbx-manage` appends to `cache_dir/events/images` after a pull, prepare, untag or delete.
Both use [`sbx-events`](../../crates/port/events/src/lib.rs): one JSON object per line, appended under a file lock,
two generations of 1 MiB. `sbx events` reads both directories, merges them by time, replays from `--since`, and
keeps following until `--until` or Ctrl-C. A VM whose owner was killed has no `stop` event, because nobody was left
to write it (it is listed in `DOCKER_DIFFERENCES`).

`--format json` prints the public types in [`output.rs`](../../crates/app/cli/src/output.rs), never the on-disk
records, so the storage format can change without breaking scripts. [cli-output.md](cli-output.md) is generated from
those types. `--jq` runs on that output in-process.

`sbx tui` ([`tui/`](../../crates/app/cli/src/tui/mod.rs)) is the first user of that contract. It never calls the
cores. It runs the same `sbx` binary as children: `ps -a`, `images` and the `executor` listings with
`--format json`, and `events --since 1h` followed in the background. A sandbox event re-reads `ps -a`, an image
event re-reads `images`. Every key becomes an `sbx` command line, shown in the status bar, and commands that need the
terminal (`exec -it`, `attach`, `logs -f`, the `:` prompt) get it handed over until they exit. `model` (keys to
command lines, no I/O) and `view` (drawing) are unit-tested without a terminal.

## The network

The VM has no network device. With `--network allow`:

1. The runtime asks the guest to start an HTTP proxy on `127.0.0.1` (`EgressListen`) and sets `HTTP_PROXY` and
   `HTTPS_PROXY` for the process. It also lends the guest a pool of connections (`EgressSlot`).
2. A program inside makes a request through the proxy: `CONNECT host:443`, or a plain `GET http://…`. The guest
   proxy ([`egress.rs`](../../crates/app/guest/src/egress.rs)) sends `Connect{host, port}` over one of the lent
   connections.
3. The owner process checks the target against the allow list
   ([`crates/core/runtime/src/egress.rs`](../../crates/core/runtime/src/egress.rs)). It resolves names on the host
   and drops loopback, private, link-local and multicast addresses, unless the rule names that IP or `localhost`.
   Then it dials and relays bytes, or refuses (the client gets 403 with the reason).

UDP, and programs that ignore the proxy settings, simply have no way out.

## Executors and their repositories

**Choosing.** [`choose.rs`](../../crates/app/cli/src/choose.rs) picks the executor for each command: `--executor`
(`SBX_EXECUTOR`, a path or an installed `NAME[@VERSION]`), then the sandbox's pin (for `start` and `restart`), then
the nearest `.sbx-executor` from the current directory up, then the default in `data_dir/executors/default`, then
`sbx-executor` on `PATH`. A sandbox created with an installed executor records its `NAME@VERSION` in the label
`io.github.toollyst.sandbox.executor`. A detached sandbox's shim is handed the chosen program (and VMM directory).
`sbx executor current` prints the choice and where it came from.

**Two shapes.** An installed executor ([`sbx-executor-install`](../../crates/spec/executor-install/src/lib.rs)) is
either self-contained, with `sbx-executor` at the top, or a **VMM package**: the VMM's files plus `.sbx-adapter`,
which names the adapter that runs it. Adapters live in `data_dir/adapters` with the same layout. For a VMM package,
`sbx` runs the highest installed version of that adapter and passes the package directory in `SBX_EXECUTOR_VMM_DIR`
(["adapters and VMM packages"](../../crates/spec/executor-proto/src/lib.rs) in the protocol). The `firecracker`
adapter looks for `firecracker` there, and keeps the default kernel next to itself
([`sbx-executor-firecracker`](../../crates/executor/firecracker/src/lib.rs) lists the lookup order). So `wick@1.18.0+wick.1`
is only the Firecracker binary of wick release `v1.18.0+wick.1`, and the adapter, the kernel and the VMM are
versioned and upgraded separately. Because the highest installed adapter wins, installing a newer adapter changes what
every VMM package that names it runs under; that adapter can only come from the repository the installed versions came
from (see Installing).

**Installing.** `sbx executor install NAME` (say, `wick`) reads each configured repository's index (`v2/index.json`, or
`index.json` if there is none) ([`sbx-executor-repo`](../../crates/spec/executor-repo/src/lib.rs)). For a repository
with keys (`repo add --key`), it also needs `v2/index.json.minisig` signed by one of them, and checks the index's
`serial` against the highest one seen (`<data_dir>/executor-repositories.seen`) and its `expires` against the clock
([`trust.rs`](../../crates/spec/executor-repo/src/trust.rs)); a repository that fails is skipped with a warning. The
name belongs to the first repository that publishes it (`--repo` picks another), and within it the newest version
for this host's platform wins. If a repository before that one failed, nothing is installed
([`unread_before`](../../crates/spec/executor-repo/src/lib.rs)): the failed one may own the name. If it is a VMM
package, the adapter's name has an owner too (the first repository in the list that publishes it): if the adapter
isn't installed, it installs the newest one for this host and protocol version from the owner only, and an adapter
already installed from a repository other than the owner (or `local`) is refused. The VMM package's own origin is
checked before any of this, so a refusal doesn't leave an adapter behind. Every install records its origin (`.sbx-origin`, the repository
name or `local`), and a name already installed from one origin is never installed from another
([`sbx-executor-install`](../../crates/spec/executor-install/src/lib.rs)).
Each download is checked against the sha256 and size in the index and unpacked into a staging directory. Before the
rename into place, `sbx-executor capabilities` (for a VMM package, the adapter given that directory) must report this
`sbx`'s protocol version and the host's architecture ([`executors.rs`](../../crates/app/cli/src/executors.rs)).
The official repository is <https://toollyst.github.io/sandbox-executors/>, published from its own repository,
[toollyst/sandbox-executors](https://github.com/toollyst/sandbox-executors) ([Releasing](testing-and-releasing.md#executors)).

To support another VMM, either write a program that implements
[`sbx-executor-proto`](../../crates/spec/executor-proto) and ship it as a self-contained executor or an adapter, or,
if the VMM has Firecracker's interface, register it as a VMM package for the `firecracker` adapter. Test a new program
the way [`conformance.rs`](../../crates/executor/fake/tests/conformance.rs) tests the fake executor, run
`cargo xtask smoke` with it installed, and publish it in any static repository. The cores don't change.
