# How it works

This page follows `sbx run --rm alpine echo hello` from the command line into a VM and back. Then it covers the
detached path, `exec` and `cp` from another process, the network, `sbx build`, and the image pipeline. Each step names the code
that does it. The doc comment there is the precise rule.

## The pieces at run time

```text
 your terminal
     │
 sbx (sbx-cli) ──────────── cache_dir: blobs, images/<id>/, layers/<diff_id>/layer.erofs,       (sbx-manage writes)
     │                                 merged/<chain_id>/rootfs.erofs, build/<key>, guest/…/initramfs, events/images/
     │  sbx-runtime ─────── data_dir:  sandboxes/<id>/{spec.json,state.json,scratch.img,owner.lock,logs/},
     │                                 events/sandboxes/, events/network/, executors/, adapters/ (the last two: sbx executor)
     │                      socket_dir: short paths for Unix sockets (macOS caps them at 104 bytes)
     │  stdin/stdout: executor protocol (line-delimited JSON)
     ▼
 sbx-executor (sbx-executor-firecracker) ── firecracker --no-api --config-file … ── KVM / HVF
                                                   │
                       control_socket ◄── vsock ──►│ microVM
                       (CONNECT 1024)              │  /init = sbx-guest (PID 1)
                                                   │  /dev/vda bottom layer (EROFS, read-only)
                                                   │  /dev/vdb scratch.img  (writable)
                                                   │  /dev/vdc… upper layers (EROFS, read-only)
```

There are three directories, each passed in by the caller. The CLI's defaults are in
[`dirs.rs`](../../crates/app/cli/src/dirs.rs), and the `SBX_DATA_DIR`, `SBX_CACHE_DIR` and `SBX_SOCKET_DIR`
environment variables override them. Two processes given the same paths share state, and different paths keep them
apart.

Both `data_dir` and `cache_dir` hold a `layout-version` file (from 0.2.1): one decimal number, the on-disk layout
that wrote them. [`layout.rs`](../../crates/app/cli/src/layout.rs) checks it whenever `sbx` starts. A missing file
(a directory 0.2.0 wrote) is layout 1 and gets written; a higher number than this `sbx` knows is refused rather than
guessed at; a lower one is migrated in place. A change to the layout bumps `layout::CURRENT` and adds a step to
`layout::migrate`. Layout 2 stores root filesystems per layer: migrating a layout 1 cache deletes each image's
`images/<id>/rootfs.<format>`, keeps the blobs and image records, and the next `run`, `start` or `prepare` builds
the layer disks from the blobs. An older `sbx` then refuses the cache, as above.

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
  `docker.io/library/alpine:latest`. The containers `registries.conf` ([`registries.rs`](../../crates/adapter/oci/src/registries.rs))
  may turn that into mirrors to try first or block it, and `policy.json` ([`policy.rs`](../../crates/adapter/oci/src/policy.rs))
  may reject it before any request or require a cosign signature on the first manifest. Credentials come from
  Docker's and podman's files ([`auth.rs`](../../crates/adapter/oci/src/auth.rs)) when the registry asks. It fetches
  the index and picks `linux/<host arch>`, checks every document's sha256, and lists the layers. `sbx-manage` downloads only the missing layers. It checks each digest while
  writing and reports progress (the CLI draws the bar). The manifest and config are kept as pulled (for `sbx save`),
  and the image record and reference are written last.
  `sbx load` runs the same steps with an OCI image layout as the source ([`layout.rs`](../../crates/adapter/oci/src/layout.rs)).
- **`push`** (the other way, `sbx push`): `sbx-manage` reads the stored manifest and config, checks that the
  manifest still hashes to the image ID and each layer file has the size the manifest says, and hands the blobs to an
  [`ImageDestination`](../../crates/port/image/src/lib.rs) one by one, the manifest last. `sbx-oci`
  ([`push.rs`](../../crates/adapter/oci/src/push.rs)) speaks the Distribution push protocol for each: `HEAD` to skip a
  blob the registry has, a cross-repository mount when the image was pulled from another repository of the same
  registry, otherwise an upload session (`POST`, then one `PUT` with the whole blob, or `PATCH` chunks that follow the
  registry's `Location` and `Range` if it answers 413), and finally `PUT` of the manifest bytes by tag. The token asks
  for `pull,push`. The name alone picks the registry: no mirror, no `location` rewrite, and `blocked` refuses. Nothing
  is tagged until every blob is there.
- **`prepare`**: each layer becomes one EROFS image, `layers/<diff_id>/layer.erofs`, shared by every image that
  has that layer. [`sbx-rootfs`](../../crates/port/rootfs) `lower` writes a layer the way overlayfs reads a lower
  directory: a whiteout is a character device 0/0, an opaque directory has `trusted.overlay.opaque=y`. Nothing is
  unpacked onto your disk, so the result is the same on macOS, Linux and Windows. The plan
  ([`stack.rs`](../../crates/port/store/src/stack.rs)) walks from the top layer down and gives each layer its own
  disk while the executor has disks left (`max_disks` minus one for scratch, at most 16). The layers below that are
  merged into one EROFS, `merged/<chain_id>/rootfs.erofs`, the same in-memory merge (whiteouts, opaque directories,
  hard links) that 0.2 did for the whole image. A layer that can't stand on its own as a lower (a hard link to a
  file in a lower layer, a whiteout of the root) is merged with everything below it. Two processes preparing images
  that share a layer build it once: each layer and merged disk has its own lock (`.lock` in its directory), and the
  files appear by rename. If the image carries its own kernel (label `io.github.toollyst.sandbox.kernel`), it is
  extracted to `images/<id>/kernel`.

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

`create` never pulls. If the image or its layer disks for this executor are missing, it fails, and the CLI is the
one that pulled and prepared in step 2. `start` prepares again if they went missing since (a cache migrated from
layout 1, or a `prepare` for an executor with fewer disks).

### 4. Start: launch the VM (`sbx-runtime` → executor)

`Sandbox::start`:

1. **Takes ownership.** It holds `owner.lock` and a shared lease on the image in the cache, so `rmi` and
   `system prune` can't delete its layer disks under a running VM. A layer is deleted only when no image uses it
   any more (`sbx_store::write::collect`, under the store's lock that `prepare` also waits on).
2. **Picks the boot files.** The guest init is the initramfs embedded in `sbx`, extracted once to
   `cache_dir/guest/sha256-<digest>/<arch>/initramfs`. The kernel is the executor's default, unless the image
   brought its own.
3. **Launches through the executor.** [`sbx-executor-process`](../../crates/adapter/executor-process) runs
   `sbx-executor run` and sends `Launch` (disks, initrd, kernel, vCPUs, memory, `control_socket`). Then it waits for
   `Started`. The executor's stderr goes to `logs/executor.log`. The disks are, in order: the bottom layer (or the
   merged disk), scratch, then the upper layers, bottom to top. Each is attached as the next virtio-blk device, so
   the order alone tells the guest what it is. The executor protocol stays at version 2: an executor only has to
   attach up to its `max_disks` disks in order.
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
2. It mounts `/dev/vda` and then `/dev/vdc`, `/dev/vdd`, … read-only, up to the first disk that isn't EROFS
   ([`disks.rs`](../../crates/app/guest/src/disks.rs): volumes come after the layers and are never EROFS). It
   formats `/dev/vdb` on first boot (scratch is zero-filled, so "no filesystem" means "new") and reuses it after a
   restart. The kernel command line doesn't change.
3. It stacks overlay(layers ↓, scratch ↑), mounts `/proc`, `/sys`, `/dev`, `/dev/pts` and `/dev/shm` inside, moves
   it to `/`, and chroots. It brings up loopback and sets the hostname.
4. It re-executes itself as the **agent**, which listens on vsock port 1024 and serves each connection on its own
   thread. It also starts a loopback DNS responder that answers NXDOMAIN at once, so name lookups without network
   fail instantly instead of after a 5 s timeout (with `--network allow` it answers from the allow list instead;
   see "The network").
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

## The guest clock

A VM's clock stops while the host sleeps and is not corrected when it wakes, and it drifts a little even without
sleep (about 0.35 s a day on the reference executor). So the owner (`sbx` for attached runs, `sbx-shim` for detached
ones) keeps the guest clock on the host's, without chrony in the image or anything more from the executor. Right
after the guest is ready, every 60 s, and as soon as it sees the host's wall clock jump, it opens a connection with
`ClockSync`. The guest sends `ClockAsk` up to four times, the host answers each with `ClockNow{its wall clock}`, and
the guest keeps the sample with the shortest round trip: the host's time at arrival is estimated as `ClockNow` plus
half the round trip, so the error is at most half of it (vsock round trips are about 0.1 ms; samples slower than
20 ms are not used). Offsets of 128 ms or more are stepped at once (as after a sleep, or when someone ran `date -s`
inside); smaller ones are slewed at the kernel's 0.5 ms per second, so the clock never runs backwards for them. A host
clock set back by more than 128 ms is followed backwards too. The owner notices a jump by waking every 0.5 s and
comparing how far the wall clock and the monotonic clock moved: on macOS and Linux the monotonic clock stops during
sleep, so the wall clock runs ahead; where it keeps running (Windows), the 0.5 s wait ends late. Either way the
guest is corrected within about half a second of waking. The first sync runs in the background and never delays
the main process. Nothing the guest replies affects the host, and failures go to `logs/executor.log`, once per kind;
a guest init too old to know `ClockSync` is left alone. Rules: [`clock.rs`](../../crates/core/runtime/src/clock.rs)
(host) and [`clock.rs`](../../crates/app/guest/src/clock.rs) (guest).

## Events, JSON output and the TUI

There is no daemon to ask what changed, so the process that changes something writes it down.
`sbx-runtime` appends a line to `data_dir/events/sandboxes` whenever it records a sandbox as created, running,
stopped or removed, and `sbx-manage` appends to `cache_dir/events/images` after a pull, build, prepare, tag, untag, push or
delete.
Both use [`sbx-events`](../../crates/port/events/src/lib.rs): one JSON object per line, appended under a file lock,
two generations of 1 MiB. `sbx events` reads both directories and the outgoing-connection records in
`data_dir/events/network`, merges them by time, replays from `--since`, and
keeps following until `--until` or Ctrl-C. A VM whose owner was killed has no `stop` event, because nobody was left
to write it (the `events` row of [`compat.rs`](../../crates/app/cli/src/compat.rs) says so).

`--format json` prints the public types in [`output.rs`](../../crates/app/cli/src/output.rs), never the on-disk
records, so the storage format can change without breaking scripts. [cli-output.md](cli-output.md) is generated from
those types. `--jq` runs on that output in-process.

`sbx tui` ([`tui/`](../../crates/app/cli/src/tui/mod.rs)) is the first user of that contract. It never calls the
cores. It runs the same `sbx` binary as children: `ps -a`, `images` and the `executor` listings with
`--format json`, and `events --since 1h` followed in the background. A sandbox event re-reads `ps -a`, an image
event re-reads `images`. Every key becomes an `sbx` command line, shown in the status bar, and commands that need the
terminal (`exec -it`, `attach`, `logs -f`, the `:` prompt) get it handed over until they exit. A child's error is
its JSON error line; the TUI shows its `message`, never the JSON. Enter shows the selected row's public type as
`name  value` lines (`detail`), in the JSON's field names and order, with the command that prints the same JSON.
`model` (keys to command lines, no I/O) and `view` (drawing) are unit-tested without a terminal.

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
   Then it dials and relays bytes, or refuses (the client gets 403 with the reason). With `--egress-limit` it stops
   relaying at the limit and refuses new connections. Each decision goes to `data_dir/events/network` (`connect`,
   `refuse`, `close` with byte counts), apart from the lifecycle events so a busy sandbox can't push them out.

**Transparent TCP** is for programs that ignore the proxy. It needs nothing from the executor or the guest kernel
beyond IPv4 loopback ([`transparent.rs`](../../crates/app/guest/src/transparent.rs) in the guest):

4. Right after lending the connections, the runtime sends `Transparent` with the IPv4 rules the guest can listen
   on. The guest adds one netlink route, `local 198.18.0.0/15 dev lo` (so it can listen on any address in that
   range), rewrites `/etc/resolv.conf` to `nameserver 127.0.0.1` in the writable layer, and listens on each IP rule
   (making that `/32` local too). Rules with a `*` port, IPv6 addresses and `localhost` are recorded as
   `proxy_only`; the CLI warns about them when the sandbox is created.
5. The guest's loopback DNS ([`dns.rs`](../../crates/app/guest/src/dns.rs)) answers `localhost` itself and asks the
   host about any other name with `Lookup{name}` over a lent connection. The host
   ([`lookup`](../../crates/core/runtime/src/egress.rs)) matches the name against the list and answers `Resolved`
   with the open ports, without resolving it, so a name you didn't allow never leaves the machine, not even as a DNS
   query. It records `lookup` or `refuse` (`via=dns`) once per name, up to 1024 names per start. The connection
   goes back to the pool.
6. For an allowed name the guest picks the next address in the range (stable for the VM's life; PTR maps it back),
   opens a listener on it for each port, and only then answers. AAAA and other types get "no records", so programs
   use IPv4. A name that isn't allowed is NXDOMAIN.
7. A connection to a stand-in address is mapped back to the name and sent as the same `Connect{host, port}` the
   proxy sends; from there step 3 is unchanged. The proxy does the same mapping if a program resolved first and then
   asked the proxy for the address.

UDP beyond DNS, rules with a `*` port for programs that ignore the proxy, and programs with their own DNS have no way
out.

**Connecting sandboxes** (`--connect db:5432`) rides on transparent TCP. Nothing listens on the host and the target
needs no option. Rules: [`connect.rs`](../../crates/core/runtime/src/connect.rs) and
[`egress.rs`](../../crates/core/runtime/src/egress.rs) in the runtime, `ConnectRule` and `check_connect` in
[`sbx-model`](../../crates/model/src/lib.rs).

1. At create time the CLI resolves each target the way other commands do (name, ID, unique ID prefix) and records it
   by name if it was given by name, by full ID otherwise. The runtime checks the list (the name is a DNS name, it is
   not also an `--allow` name, it does not point at the sandbox itself), that every target exists, and that it
   carries the labels in `connect_scope` (`sbx mcp` sets the MCP owner label there).
2. At start the owner lends connections and sends `Transparent` even with `--network none`, but without
   `EgressListen`: there is no proxy and the allow list is empty, so only the `--connect` names resolve.
3. `Lookup{db}` is answered from the allow list and the connect list together; the guest gives `db` a stand-in
   address and listens on the connected ports only.
4. A connection arrives as `Connect{db, 5432}`. If it matches a connect rule, the owner looks the target up again,
   by exact ID or by name (never by prefix, so a vanished name can't land on another sandbox), and refuses it if it
   is missing, is the sandbox itself, lacks a `connect_scope` label, is not running (its owner lock tells), or if
   the port is the target's egress proxy. Otherwise it opens the target's control socket from `state.json`, as
   `exec` does, sends `Forward{port}` (the `-p` path: the target's guest dials its `127.0.0.1:port`) and, once that
   answers `Done`, answers `Connected` and relays `Data`/`End` frames between the two guests.
5. Bytes count against the connecting sandbox's `--egress-limit` in both directions, and the connection holds one
   of its open-connection places. `connect`, `refuse` and `close` events carry `target` and `target_id`.

Because the lookup happens per connection, a restarted target (new control socket) is reached on the next
connection, and a stopped one fails that connection only, within the time it takes to read its records.

## `sbx build`

Four crates take part, and only the CLI knows all of them
([`app/build.rs`](../../crates/app/cli/src/app/build.rs)):

1. **Plan** ([`sbx-dockerfile`](../../crates/adapter/dockerfile/src/lib.rs)). The Dockerfile and the options
   (`--build-arg`, `--target`, the host's architecture) become a `BuildPlan` (`sbx_model::build`): stages and
   steps with every variable already expanded. Dockerfile types never leave that crate. A base image's `Env` changes
   what `$PATH` expands to, so the CLI plans twice: once to learn the `FROM` images, which it then finds or pulls
   through `sbx-manage`, and again with their `Env`.
2. **Run the plan** ([`sbx-build`](../../crates/core/build/src/lib.rs)). The core walks the stages in order and
   sees the world through three traits ([`ports.rs`](../../crates/core/build/src/ports.rs)): `Store` (images, the
   cache, layers), `Machines` (start a build VM on a list of layers) and `Vm` (open a layer, run, export, put
   files). For each step it computes a cache key ([`key.rs`](../../crates/core/build/src/key.rs)):
   `sha256` over the key below, the step's text and its extras (the `ARG`s a `RUN` sees, the content hash of what a
   `COPY` copies, the stage or image it copies from). A hit reuses the recorded layer and its time.
3. **Layers.** A `RUN` that misses starts the stage's VM if it isn't running yet, on the stage's layers so far. In
   the guest, `OpenLayer` stacks a new empty writable overlay layer on top, `ExecStep` runs the command there (with
   the step's env, workdir and user; the guest ends whatever it left behind), and `ExportLayer` sends the layer's
   changes back as entries, which the host writes as an uncompressed OCI layer tar
   ([`runtime/src/layer.rs`](../../crates/core/runtime/src/layer.rs) checks them as hostile input). `COPY`, `ADD`
   and `WORKDIR` never need the VM: [`source.rs`](../../crates/core/build/src/source.rs) finds the sources (context
   or a merged tree of another stage's or image's layers), [`copy.rs`](../../crates/core/build/src/copy.rs) places
   them by Docker's destination rules against the merged tree of the layers below, and
   [`tarw.rs`](../../crates/core/build/src/tarw.rs) writes the tar. If the VM is running, the same entries are also
   put into its open layer (`CopyIn`), so later `RUN`s see them.
4. **Store** ([`sbx-manage`](../../crates/core/manage/src/build.rs)). `add_layer` reads each tar with the same
   reader as a pulled layer and turns it into a layer disk under `layers/<diff_id>/`. There is no compressed blob:
   a built layer is only its disk (a layer that can't stand alone, such as a hard link to a file in a lower layer,
   keeps its tar as an uncompressed blob and is merged with the layers below). A tar with no entries is the well-known empty layer, which takes no disk
   and is skipped when stacking. `commit_built` writes the OCI config the core assembled
   ([`config.rs`](../../crates/core/build/src/config.rs), with BuildKit's history entries); the image ID is that
   config's `sha256`, and the image is recorded with `source = "sbx build"`.

The build VM is an ordinary sandbox with no main process (`hold`), the label `io.github.toollyst.sandbox.build=1`, a
16 GiB sparse writable disk and the build's network policy. Its clock is kept on the host's like any sandbox's
(see [The guest clock](#the-guest-clock)), so `RUN` steps check TLS certificates against the right time. The clock
thread opens its own guest connections at any moment, even between `OpenLayer`, `ExecStep` and `ExportLayer`: that is
safe because `ClockSync` touches no file system, while the guest's switch to a new layer changes the root for every
request that does (the order rule in [`layer.rs`](../../crates/app/guest/src/layer.rs)). The start checks the layer
count (`CheckLayers`) before the clock thread starts, so a wrong disk count stops the VM before anything else talks
to it. Its root is a hidden image record (`sbx build (working
root)`) holding the stage's layers so far, removed with the VM when the stage ends, even on failure. Refused names for a failed
step's `blocked:` lines come from the network log (`refuse` events of that sandbox).

**Cache records** live in `cache_dir/build/<key>`, one small JSON file each (`layer` and `created`). They hold
layers alive: the collector in `sbx-store` keeps every layer and blob a record names, so `sbx rmi` of the last
image on a layer leaves the disk while the cache can reuse it. `sbx system prune` removes the records and stale
build roots first, then collects. There is no size limit or age-based eviction yet; `system df` shows the cache.
While a build runs it holds claims on the layers it has made or reused, so a concurrent prune doesn't delete them
before the image or record is written.

A built image has layer disks but no layer tars or manifest, so `save` and `push` refuse it
(`ManageError::NotExportable`) until the export step that writes tars from the disks.

The fake executor ([`sbx-executor-fake`](../../crates/executor/fake/src/lib.rs)) plays the build VM for tests: on
the first `OpenLayer` it unpacks the EROFS layer disks into a directory on the host and moves every guest path under
it, so the e2e tests run whole builds without a VM (commands run on the host, so they use relative paths).

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
