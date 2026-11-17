# Changelog

What changed in each release, for people who use `sbx` and script against it. The rules for what counts as breaking
are in [COMPATIBILITY.md](COMPATIBILITY.md); fixes for vulnerabilities are listed under **Security**
([SECURITY.md](SECURITY.md)).

Each release has a section `## vX.Y.Z`; the release workflow publishes that section as the release notes and refuses
to release a tag without one. Changes land under `## Unreleased` first, and the section is renamed when tagging.

## v0.3.0

0.3 is about the development loop: `sbx build` makes images from a Dockerfile in microVMs, `sbx push` uploads them, named
volumes keep a database's data, `--connect` lets one sandbox reach one port of another, and programs that ignore
proxy settings get through the allow list too. Every sandbox and every build stage is still its own VM with no
network device. Nothing that worked in 0.2 needs a different command, but the on-disk layout changes and 0.2.x can't
read it afterwards: read **Upgrading from 0.2** first.

### Upgrading from 0.2

- The cache and data directories move to layout 2 the first time this `sbx` uses them. Each image's merged
  `rootfs.erofs` is deleted, and the next `run`, `start` or `prepare` builds the per-layer disks from the layers
  already pulled. After that, `sbx` 0.2.x refuses both directories. To go back to 0.2.x, point it at other
  directories (`SBX_DATA_DIR`, `SBX_CACHE_DIR`).
- Volumes need an executor that speaks executor protocol 3. `sbx` speaks 2 and 3, so an installed executor keeps
  working for everything else; with one that speaks only 2, a sandbox that asks for a volume is refused with that
  reason. The `firecracker` adapter of this release speaks 3.
- Sandboxes started by 0.2 keep running and still work with `exec`, `cp`, `logs`, `stop` and `rm`. Restart them to
  get what this release adds inside the guest (clock correction, transparent TCP, `--connect`).
- With `--network allow` or `--connect`, `sbx` now writes the sandbox's `/etc/resolv.conf` (in its writable layer;
  the image is unchanged) to point at the guest's own DNS, as Docker does with its embedded DNS.

### Added

- `sbx build` makes an image from a Dockerfile, with buildx's flag names (`-t`, `-f`, `--build-arg`, `--target`,
  `--no-cache`, `--progress auto|plain`, `-q`, and `--cpus`, `-m` for the build VM). Each stage's `RUN` steps run in
  one microVM with the network closed unless `--network allow --allow HOST:PORT` opens it. When a step fails, `sbx`
  lists the names it was refused, with the `--allow` to add (`blocked: NAME — add --allow NAME:PORT`), and with the
  default closed network a line saying the network is closed. `COPY`, `ADD` and `WORKDIR` layers are made on the
  host, honoring `.dockerignore`, so a cached rebuild or a `FROM scratch` stage of copies starts no VM. Every step
  that makes a layer is cached; built images share layer disks with the images they are built on. `--format json`
  prints `BuildEvent` JSON Lines, and an image event `build` is recorded when the image is kept in sbx. `ADD` of an
  archive, `ONBUILD`, `RUN --mount`, heredocs and other platforms are errors with the line number. See
  [Build](docs/guide/usage.md#build) in the usage guide and the build rows of the
  [compatibility table](docs/guide/tool-compatibility.md#build).
- `sbx build --output` (`-o`, repeatable) says where the image goes: `type=sbx` (the store, the default),
  `type=docker` (into `docker load`, or `dest=FILE`; `--load`), `type=oci,dest=FILE|DIR/|-` (an OCI archive or
  layout), `type=registry` (`--push`). Given outputs replace the default: `--load` alone keeps nothing in sbx, but the
  layer cache stays. `-f -` reads the Dockerfile from stdin. `--format json` adds `stored` to the `built` line and an
  `exported` line per output.
- Built images can be saved and pushed. Layers are written as tars again when exported and gzipped with fixed
  settings; a layer's compressed digest is recorded on its first export (a few bytes next to the layer; the
  compressed layer is never stored), so every `save`, `--load`, `-o type=oci` and push of an image has the same
  manifest digest.
- `SOURCE_DATE_EPOCH` (environment or `--build-arg`) sets a built image's `created` and history times and caps file
  times in the layers the build makes, as in BuildKit.
- The build cache keeps itself under `SBX_BUILD_CACHE_MAX` (default `10g`, `none` for no cap) after every build,
  dropping the least recently used steps whose layers no image uses. `sbx builder prune [-a] [-f] [--keep-storage
  SIZE]` empties it (JSON `BuildCachePruned`). `sbx system df` shows it as `build cache`, and `sbx system prune`
  removes it (JSON `build_cache` in the prune report).
- A stored built image remembers where it was built: the context directory and Dockerfile as absolute paths, and the
  `--target` (no build arguments, no `--allow` rules). `images --format json` shows it as `build` (JSON `ImageBuild`;
  `file` is null for `-f -`); pulled and loaded images have none. The last build that stored an image ID wins; a
  build that stores nothing changes nothing. The paths stay in the local image record and are never written into the
  image or anything `save`, `push` or `--output` writes.
- `sbx tag SOURCE TARGET` and `sbx push [-a] [-q] NAME[:TAG]`. `push` sends the layers, the config and last the
  manifest, exactly as pulled or loaded, so the registry reports the image's own digest. A layer the registry has is
  not sent, one from another repository of the same registry is mounted, and a registry that refuses a whole upload
  (413) gets it in chunks. `push` never goes through a `registries.conf` mirror or rewritten location and refuses a
  `blocked` registry. `--format json` prints one `Pushed` line per tag, with what happened to each layer (`exists`,
  `mounted`, `uploaded`). Image events `tag` and `push` are recorded (`events --filter event=tag|push`). In
  `sbx tui`, `t` and `p` on the Images tab fill them in.
- Named volumes: `sbx volume create [--opt size=50g] [--label k=v] NAME`, `volume ls [-q] [--filter]`,
  `volume inspect`, `volume rm [-f]` and `volume prune -a`, and `run`/`create` take `-v NAME:/path[:ro|:nocopy]` and
  `--mount type=volume,source=NAME,target=/path[,readonly][,volume-nocopy]`. A name used for the first time creates
  the volume (16 GiB, sparse). Each volume is a disk of its own in `data_dir/volumes/`, formatted by the guest (ext4
  with a journal) on first use and filled with what the image has at that path; it outlives the sandbox (`rm` keeps
  it). One running sandbox at a time attaches a volume read-write (a second is refused with the name of the one that
  has it); `:ro` attaches share it while nobody writes. `fsync`ed writes survive the VM being killed. `-v /path`
  alone (an anonymous volume) is refused. `sbx system df` shows volumes, `sbx system prune -a --volumes` removes
  unused ones, `sbx events` records volume `create` and `remove`, shell completion offers volume names, and
  `sbx tui` has a Volumes tab. `--format json` adds `Volume`, `VolumeRef` and `VolumePrune`, `volumes` in `Sandbox`
  and in the prune report. MCP tools `run` and `create` take `volumes`; they only attach volumes created through MCP
  (`sbx mcp --max-volumes`, 8 by default).
- Executor protocol 3: the disk role `volume`, whose writes the executor must flush through to the host file. The
  `firecracker` adapter speaks 3 and attaches volume disks with `cache_type: Writeback`.
- `sbx run|create --connect [NAME=]SANDBOX:PORT` (repeatable, `db:5432,6379` for more ports): programs inside reach
  TCP `PORT` of another running sandbox in the same data directory by its name, with any protocol and without proxy
  settings (`sbx run --connect db:5432 postgres:17 psql -h db`). Nothing else opens: with `--connect` alone no other
  name resolves and there is no proxy; it combines with `--network allow`, and a `--connect` name may not also be an
  `--allow` name. The target needs no option and nothing listens on the host. The target is looked up at each
  connection, so a restarted (or recreated, by name) target works on the next one and a stopped one fails that
  connection at once; `connect`, `refuse` and `close` network events carry `target` and `target_id`, and
  `events --filter sandbox=` matches either side (and now matches network events by sandbox name too). The bytes
  count against the connecting sandbox's `--egress-limit`, which `--connect` alone now accepts. `inspect` and
  `ps --format json` show `network.connect`, and so does the `sbx tui` detail view; shell completion offers sandbox
  names. The MCP tools `run` and `create` take `connect`, limited to sandboxes created through MCP (checked at
  creation and at every connection). `sbx build` has no `--connect`.
- Transparent TCP: with `--network allow`, programs that ignore `HTTP_PROXY` (`git` over ssh, `psql`, a Go binary)
  reach `HOST:PORT` rules too. The guest's DNS asks the host whether a name is on the list, and the host answers from
  the list alone, without resolving it, so a name you didn't allow never leaves the machine, not even as a DNS query.
  An allowed name gets a stand-in address from `198.18.0.0/15` that the guest listens on for the rule's ports; the
  host dials the real name after the same checks as the proxy. A name that isn't allowed fails at once and is
  recorded. Rules with a `*` port, IPv6 addresses and `localhost` stay proxy-only, and `sbx run` warns about each.
  New `network` events: `lookup` (an allowed name and its open ports), `refuse` with `via=dns`, `proxy_only` and
  `notice`. It needs nothing from the executor or the image.
- Live shares (`-v ./dir:/path`) pass host edits to file watchers in the sandbox, so hot reload (vite, webpack,
  nodemon) works: the process that owns the VM watches each writable share on the host and the guest raises an inotify
  "modified" event on each changed file, or on the nearest existing folder of a removed one (there are no create or
  delete events, and none on a `:ro` share). Failures go to the sandbox's `logs/executor.log`. This works with any
  executor that announces `live_share`; the `firecracker` adapter doesn't yet (upstream Firecracker has no virtio-fs,
  and wick's is designed but not built).
- The clock inside a sandbox follows the host's. The process that owns the VM (`sbx`, or `sbx-shim` for `-d`) sets
  the guest clock right after it boots, corrects it every minute (slowly, so it never runs backwards), and steps it
  as soon as the host's clock jumps, such as after the host wakes from sleep (until now the guest fell behind by the
  time the host slept). It needs nothing from the image or the executor and never delays the start. A clock set
  inside with `date -s` is put back at the next correction.
- Install and run, nothing in between, once the official repository is signed (see **Notes**). With no executor
  installed, `sbx run`, `create`, `start` and `build` install the default executor from the official repository first
  and carry on, saying on stderr what they install and from where (never on stdout, so `--format json` and `sbx mcp`
  stay clean). Which executor is the default is said by the repository's index (a new optional `default` field in
  schema 2, written by `cargo xtask repo-add --default`), not by `sbx`. It only uses a signed `official`
  repository, checked exactly like `sbx executor install`, so it asks nothing, also without a terminal. Two first
  runs at once install it once; an unreachable repository fails with the reason and the manual ways. It never
  happens when any executor is installed or `--executor` is given, and `SBX_NO_AUTO_EXECUTOR=1` or
  `--executor none` (`SBX_EXECUTOR=none`) turns it off. `sbx executor current`, `sbx info` and `sbx tui` say whether
  it will happen and why not, and say to `sbx executor use` one when executors are installed but none is the
  default.
- The installers have their own version, separate from `sbx`'s and the executor's. Each version pins the `sbx`
  release and the executor (with its adapter) it installs, prints them as its first line, and shows them without
  installing with `--version` (`curl … | sh -s -- --version`). Every version stays published unchanged as
  `install-<version>.sh` and `.ps1`; `install.sh` and `install.ps1` are copies of the newest. `install.sh` also
  installs the pinned executor from the official repository, unless you already use another one or set
  `SBX_EXECUTOR=none`; if that fails, `sbx` is still installed.
- `sbx images` shows `SIZE`, `SHARED` and `UNIQUE` (on disk), and `--format json` adds `disk_size`, `shared_size`
  and `unique_size`. `sbx system df` counts a shared layer once and shows the shared part (`SHARED`, JSON `shared`).
  The Images tab in `sbx tui` shows `SIZE` and `SHARED`.
- `sbx tui` builds from the Images tab: `b` fills in `build -t `, and `R` on an image built here (marked `*`) fills in
  `build -t NAME -f FILE [--target T] CONTEXT` from that record; Enter hands the terminal to the build so its progress
  shows. The image details show the record. An empty Images tab suggests `b`, image `build` events show in green, and
  `B` runs `builder prune`.
- A compatibility table, [tool-compatibility.md](docs/guide/tool-compatibility.md): every command and flag of the
  Docker CLI, Podman and nerdctl, every Dockerfile instruction, the common Compose keys, and the features of Docker
  Desktop, Podman Desktop, OrbStack, Colima and Rancher Desktop (each product cell links the product's own
  documentation), with what `sbx` does for each, why, and the test that shows it. It is generated from
  `crates/app/cli/src/compat.rs`, tests check every row against the command-line definition, and the README shows
  the progress for each target.

### Changed

- Images are stored per layer: each layer becomes one read-only EROFS disk, shared by every image that has it, and
  the VM stacks them as overlay lower layers. A second image on the same base only adds its own layers, on disk and
  in `prepare` time. When an image has more layers than the executor can attach disks, its lowest layers are merged
  into one disk (an executor with 2 disks gets one merged disk, as before). Executors attach the disks in order as
  before, and the kernel command line is unchanged.
- `sbx system prune` also removes layer disks no image uses.
- With an executor that can't share live, the error for `-v ./dir:/path` now names the alternatives: the
  `--mount type=copy,source=…,target=…` line to paste, or an executor that announces live sharing (`sbx info`).
- The installers install the `sbx` release their version pins, not the latest release; `SBX_VERSION` still picks
  another. The Sigstore signature must now always come from exactly the tag installed. Releasing `sbx` no longer
  publishes the installers; a new installer version is published by hand once the pair is tested
  (`installer.yml`).

### Fixed

- The official `postgres` image didn't start: the guest's `/dev` now has the `/dev/fd`, `/dev/stdin`, `/dev/stdout`
  and `/dev/stderr` links that OCI runtimes add, which its entrypoint and other images use.
- The `firecracker` adapter no longer leaves Firecracker running when its watcher process alone is killed
  (`kill -9`, the OOM killer): the watcher runs in its own process group, which the adapter kills before it reports
  the VM gone.
- `sbx executor rm`, `sbx executor adapter rm` and `sbx executor prune` no longer remove an executor, VMM package or
  adapter that a running sandbox was started with: `rm` refuses and names the sandbox (`-f` removes it with a
  warning), and `prune` keeps it and says so. `executor ls` and `executor adapter ls` add `running` (JSON) and the
  Executors tab of `sbx tui` a RUNNING column. Until now `prune` could remove the older adapter version a running
  VM used.
- `sbx system df` counts the executors and adapters that running sandboxes were started with as active (it always
  showed 0).

### Notes

- The guest protocol gains messages for builds, transparent TCP, the clock and live-share changes; its version stays
  2, so a sandbox started by 0.2 keeps answering the requests it knows.
- This `sbx` doesn't carry the official executor repository's public key yet, so it treats `official` as unsigned
  and doesn't install the default executor on its own; `sbx info` and `sbx executor current` say so. Install an
  executor with `sbx executor install` or from a directory (`--from`); see the
  [install guide](docs/guide/install.md#the-executor).

## v0.2.2

Only additions: `sbx` tells you about newer releases and upgrades itself, checking signatures.

### Added

- `sbx` tells you when a newer release is out: after a command, one line on stderr such as
  `sbx 0.2.1 (0.2.2 available): run 'sbx upgrade'`, at most once a day (every command when the newer release fixes a
  vulnerability or yours has been yanked). It reads a signed release index at most once a day, in the background, and
  waits for it at most 3 seconds. It sends nothing but the request and `User-Agent: sbx/<version> (<os>; <arch>)`, and
  never runs in CI, with `--format json`, without a terminal, or with `SBX_NO_UPDATE_CHECK=1`. `sbx --version` (in a
  terminal), `sbx info` (first line, and `update` in its JSON) and the `sbx tui` header show the same.
- `sbx upgrade [--check] [--version V] [-y] [--allow-downgrade]` replaces `sbx` and `sbx-shim` with a release whose
  minisign signature matches the key built into `sbx` (and whose Sigstore signature verifies when `cosign` is
  installed; `SBX_VERIFY` works as in the installer). It asks before changing minor version, installing a yanked
  release or one your executors may not speak; it never moves to an older data layout; it leaves installs from a
  package manager to that manager. `--check` exits 10 when a newer release exists. `V` in `sbx tui` runs it.
- Releases ship `sbx-windows-x86_64.tar.gz` next to the zip, and a `.minisig` for every archive.

### Notes

- 0.2.1 doesn't have this yet: run the installer once more to get 0.2.2, then `sbx upgrade` from there on.
- Release signing keys (minisign public keys built into `sbx`; the first of each pair is in use, the second is the
  spare):
  - archives: `RWSr7ktMwZC4t4vgBvqOe2Km++mDzKImzELNmRcj7LGgbEj5BpJGunRs`,
    `RWT2l04k8kMiFHbILLsjw7ZNl613AmkjHaJO50zUuKQfFVfuJoCEzZ3r`
  - release index: `RWQErLK0V41d0FB9dXtZKU7wcQrONLepSj9s4cq0wbKjWHmsti8NyKXD`,
    `RWTX1Xl5hIj8pN0bOHaD/GizQjAN/H6IoznrKHINhT2dgNRe73NRGvoF`

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

### Fixed

- The output a sandbox's main process writes just before it exits is no longer sometimes lost: the guest could
  stop reading the pipe after a quiet poll if the process ended during that poll.

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
