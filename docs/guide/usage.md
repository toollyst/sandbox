# Use it like Docker

`sbx` takes the commands and flags you know from `docker`. Where it differs, it's on purpose: the
[Docker compatibility table](docker-compatibility.md) lists every Docker command and flag, what sbx does with it, and
why.

- [Run and manage sandboxes](#run-and-manage-sandboxes)
- [Files](#files)
- [Network](#network)
- [Images](#images)
- [Build](#build)
- [Resources](#resources)
- [Scripting](#scripting)
- [Full-screen view](#full-screen-view)

## Run and manage sandboxes

```sh
sbx run --rm alpine echo hello                       # attached, removed afterwards
sbx run --rm -it alpine sh                           # interactive shell with a TTY
sbx run --rm --cpus 2 -m 1g python:3.12-alpine python -c 'print(42)'

sbx run -d --name dev alpine sleep 600               # detached
sbx exec -it dev sh                                  # a name, an ID, or an ID prefix
sbx exec -d dev sh -c 'make serve'                   # in the background; returns at once
sbx exec --timeout 5m dev make test                  # killed after 5 minutes (exit 124)
sbx cp dev:/etc/alpine-release .
sbx logs -f dev
sbx logs -t --since 10m dev                          # with timestamps, the last 10 minutes only
sbx stop dev && sbx rm dev

sbx rm -f $(sbx ps -aq)                              # clean up everything
```

`sbx stats` shows each running sandbox's CPU, memory, writable disk and process count as its guest reports them,
refreshing every second on a terminal (`--no-stream` for one sample; one sample without a terminal and with
`--format json`).

The lifecycle commands are `create`, `start [-a] [-i]`, `stop [-s SIG] [-t SECS]`, `restart`, `kill [-s SIG]`,
`wait`, `attach` (detach with Ctrl-P Ctrl-Q) and `rm [-f]`. A stopped sandbox keeps its disk, so `start` picks up
where it left off. `-e`, `--env-file`, `-w`, `-u`, `--name`, `--label` and `--entrypoint` work as in Docker.

Exit codes follow Docker: 125 when `sbx` fails, 126 when the command can't run, 127 when it isn't found, and the
command's own code otherwise. `exec --timeout` exits 124 when it had to kill the command, like `timeout(1)`.
`exec -d` discards the command's output; redirect it to a file inside the sandbox to keep it. A sandbox started by
an older `sbx` needs a restart before `exec -d` works in it.

The clock inside a sandbox follows the host's, as it does in a container: `sbx` sets it at start, keeps it within
milliseconds while it runs, and corrects it within about a second after the host wakes from sleep. Setting it
inside (`date -s`) only lasts until the next correction, at most a minute later. No setting or image change is
needed. Clock problems, if any, are noted in the sandbox's `logs/executor.log`.

## Files

Copy a project in when the sandbox starts, and copy results out:

```sh
sbx run --rm --mount type=copy,source=./proj,target=/work -w /work \
    python:3.12-alpine python main.py
sbx cp dev:/out/result.json ./result.json
```

`cp` from a sandbox never writes outside the destination you name: the host picks the top-level name, and names that
are paths or devices on the host are refused.

Live sharing (`-v ./dir:/work[:ro]`) needs an executor with virtio-fs. The Firecracker executor doesn't have it yet,
and `sbx` says so instead of silently copying.

## Volumes

A volume keeps data after its sandbox is gone, such as a database's files:

```sh
sbx volume create pgdata                       # optional: run creates a name it hasn't seen
sbx run -d --name db -v pgdata:/var/lib/postgresql/data -e POSTGRES_PASSWORD=x postgres:17
sbx rm -f db                                   # the sandbox goes, pgdata stays
sbx run -d --name db -v pgdata:/var/lib/postgresql/data -e POSTGRES_PASSWORD=x postgres:17
sbx volume ls                                  # size, what it uses on disk, the sandboxes that point at it
sbx volume rm pgdata                           # refused while a sandbox (even a stopped one) points at it
```

- `-v NAME:/path[:ro]` and `--mount type=volume,source=NAME,target=/path[,readonly]` attach one. A source that starts
  with `/`, `.` or `~` (or has a `/` in it) is a host path; anything else is a volume name. `-v /path` alone (an
  anonymous volume in Docker) is refused: name it.
- Each volume is a disk of its own, a sparse file under the data directory (`volumes/<name>/disk.img`), 16 GiB by
  default (`sbx volume create --opt size=50g NAME`); the host only spends what is written. The guest formats it (ext4
  with a journal) the first time a sandbox attaches it, and fills it with what the image has at that path, keeping
  owners and modes, so an image that ships files or a directory owned by its service user works as in Docker.
  `nocopy` (`-v NAME:/path:nocopy`, `volume-nocopy` in `--mount`) skips that.
- A volume attached read-write is used by **one running sandbox at a time**: a second one is refused with the name of
  the sandbox that has it, until that one stops. Read-only attaches (`:ro`, enforced on the host too) can share a
  volume while nobody writes it. A sandbox whose owner was killed releases its volumes once its VM has powered off;
  until then (usually a moment) a new attach waits up to 3 seconds.
- Writes reach the disk when the program inside asks for it (`fsync`), so a database that committed a transaction
  keeps it even if the VM is killed. Each volume uses one of the executor's disks: with the Firecracker executor's 8,
  a sandbox takes up to 6 volumes (fewer leaves more disks for the image's layers). An executor that speaks an
  executor protocol older than 3 can't attach volumes, and `sbx` says so.
- `sbx volume prune -a` removes every volume no sandbox points at (without `-a` there is nothing to remove, since
  sbx makes no anonymous volumes), and so does `sbx system prune -a --volumes`. `sbx volume inspect` shows the disk
  file, the sandboxes that point at the volume and whether one has it attached now. `sbx events` records volume
  `create` and `remove`. The Volumes tab of `sbx tui` lists them (`c` create, `D` rm, `P` prune).

## Network

The VM never gets a network interface. With `--network allow`, every outgoing TCP connection is handed to the host
over vsock, and the host checks it against your list:

```sh
sbx run --rm --network allow \
    --allow pypi.org:443 --allow files.pythonhosted.org:443 \
    python:3.12-alpine pip install requests
```

A rule can be `host:port`, `*.example.com:443`, `10.0.0.5:*` or `[::1]:80`. A name that resolves to a loopback or
private address is refused unless the rule names that IP, or `localhost`, explicitly. Without `--network allow`, name
lookups fail at once instead of hanging, and nothing else changes.

There are two ways out, and both go through the same list:

- **The proxy.** Programs that follow `HTTP_PROXY`/`HTTPS_PROXY` (curl, pip, npm, apt, cargo) use an HTTP proxy
  inside the guest, which sbx sets for every process.
- **Transparent TCP.** Programs that ignore the proxy (`git` over ssh, `psql`, a Go binary that opens its own
  sockets) still reach a `HOST:PORT` rule. The guest's DNS asks the host whether the name is on the list; the host
  answers from the list alone and never resolves a name you didn't allow. An allowed name gets a stand-in address
  from `198.18.0.0/15`, which the guest listens on for the rule's ports; the program connects to it and the host
  dials the real name. A name that isn't allowed gets "no such name" at once, and the host records it, so you can see
  what to add:

```sh
sbx run --rm --network allow --allow db.example.com:5432 postgres:17 psql -h db.example.com -U app -c 'select 1'
sbx events --since 10m --until 0s --filter type=network --filter event=refuse
```

For this, sbx points the sandbox's `/etc/resolv.conf` at the guest's DNS (in the sandbox's writable layer; the image
is untouched), like Docker does. Some rules can only be reached through the proxy, and `sbx run` warns about each:
a `*` port (the guest can't listen on every port: write the port, e.g. `github.com:22`), an IPv6 address (the
sandbox has no IPv6), and `localhost` or a loopback address (inside the sandbox that is the sandbox itself). UDP
beyond DNS has no way out, and neither do programs that bring their own DNS (DNS over HTTPS, a hard-coded resolver).

`--egress-limit SIZE` (e.g. `500m`) caps how many bytes all outgoing connections send and receive together in each
start; at the limit the open ones are cut and new ones refused. Every decision is recorded as a `network` event:
`connect` (with the address reached), `refuse` (with the reason; `via=dns` and no port when the name was refused at
lookup), `close` (bytes sent and received), `lookup` (an allowed name and its open ports), `proxy_only` (a rule only
the proxy can reach, and why) and `notice` (transparent TCP could not be set up, or lookups stopped being recorded):

```sh
sbx events --since 1h --until 0s --filter type=network --filter sandbox=web
```

These records live apart from the sandbox lifecycle events, so a busy sandbox can't push those out; like them, the
most recent 1-2 MiB are kept.

`-p` publishes a TCP port of the sandbox on the host, with Docker's syntax:

```sh
sbx run -d --name web -p 8080:80 nginx          # http://127.0.0.1:8080 reaches port 80 inside
sbx run -d -p 3000 node:22 node server.js       # a free host port; `sbx inspect` shows it under ports
sbx run -d -p 0.0.0.0:8080:80 nginx             # reachable from other machines too
```

Unlike Docker, a mapping without an address listens on `127.0.0.1` only. The service inside must listen on its
loopback (or on all addresses). It works with `--network none`: published ports don't let the sandbox reach anything.
The process that owns the VM (`sbx` for an attached sandbox, `sbx-shim` for a detached one) does the listening, so a
port that is already in use makes the start fail. A sandbox started by an older `sbx` needs a restart first.

### Connecting sandboxes

`--connect SANDBOX:PORT` lets the programs in one sandbox reach one TCP port of another running sandbox, by its
name. A development database, for example:

```sh
sbx run -d --name db -e POSTGRES_PASSWORD=dev postgres:17
sbx run --rm --connect db:5432 -e PGPASSWORD=dev postgres:17 psql -h db -U postgres -c 'select 1'
```

Inside the second sandbox, `db` resolves (through the same guest DNS as transparent TCP) to a stand-in address that
listens on 5432 only. Each connection goes to the process that owns the VM, which looks `db` up, opens the target's
guest connection and joins it to `127.0.0.1:5432` inside `db`, the same way `-p` reaches a port. Any TCP protocol
works (postgres, redis, gRPC, HTTP), and the client doesn't need to know about proxies.

- **Nothing else opens.** With `--connect` alone, no other name resolves and there is no proxy. Other ports of `db`
  and every other sandbox stay unreachable. Add `--network allow --allow ...` for the outside world; a `--connect`
  name may not also be an `--allow` name.
- **The target needs nothing.** No flag, no network, no `-p`. Its service must listen on its loopback (or on all
  addresses), as for `-p`. The connection never touches a host port.
- **Looked up at each connection.** Stop `db` and the next connection fails at once (the program sees the connection
  close) and a `refuse` event records why; start it again, or recreate it under the same name, and the next
  connection works. The target must exist when you create the connecting sandbox.
- **Names.** `pg=db:5432` uses the name `pg` inside. `db:5432,6379` opens two ports. A target given by ID (or a unique
  ID prefix) is recorded by its full ID and keeps its own name inside; give one with `NAME=` if it has none. A
  sandbox cannot connect to itself or to its egress proxy port inside the target.
- **Same data directory only.** Sandboxes under another `--data-dir` are not visible.
- **Counting.** The bytes count against the connecting sandbox's `--egress-limit` (both directions; the target's own
  limit is not touched), and its connections count toward its open-connection cap.

`sbx inspect` and `ps --format json` show the list under `network.connect`. The `network` events (`lookup`,
`connect`, `close`, `refuse`) carry `target` (as recorded) and `target_id`; `--filter sandbox=` matches either side:

```sh
sbx events --since 1h --until 0s --filter type=network --filter sandbox=db
```

A sandbox created through `sbx mcp` can only connect to other sandboxes created through MCP ([MCP](mcp.md)).
`sbx build` has no `--connect`.

## Images

`pull` (with a progress bar), `prepare` (build the disk images ahead of time), `images [-q]` and `rmi [-f]`. `run`
pulls and prepares on first use (`--pull=missing|always|never`). Images come from Docker Hub, GHCR or any OCI
registry, unmodified. Each layer is converted once into a read-only EROFS disk image and cached, so after the first
run, starting a sandbox is just booting a VM. Images built on the same base share its layer disks: a second image on
`python:3.12` only adds its own layers. `images` shows `SIZE` (on disk, everything the image uses), `SHARED` (the
part other images use too) and `UNIQUE` (what `rmi` would free). An image with more layers than the executor can
attach disks gets its lowest layers merged into one disk. `images --format json` also lists the ports and volumes an image declares
(`exposed_ports`, `volumes`); `sbx` doesn't open ports or create volumes, and those paths stay on the sandbox's disk.

Without a registry, `load` and `save` move images as OCI image layout archives, the format that `docker save` (Docker
25 or later), `podman save --format oci-archive` and `skopeo copy … oci-archive:` write:

```sh
docker save alpine:3.20 | sbx load            # or: sbx load -i alpine.tar (a .tar.gz or an unpacked directory too)
sbx load -i tool.tar --tag local/tool:dev     # name an archive that holds one image
sbx save -o alpine.tar alpine:3.20            # docker load, podman load and sbx load all read it back
```

`load` checks every digest just like `pull`, and reads only the layout files out of a tar (no links, nothing outside
it). `save` writes the manifest, config and layers exactly as they were pulled, so loading the archive anywhere
gives the same image ID. An image pulled by `sbx` older than 0.2.1 has to be pulled again before it can be saved. In
`sbx tui`, `I` and `O` on the Images tab fill in `load -i` and `save -o`.

`tag` adds a name to an image, and `push` uploads it to the registry that name points at, as with Docker:

```sh
sbx tag alpine:3.20 ghcr.io/me/alpine:3.20    # a reference that pointed at another image moves
sbx push ghcr.io/me/alpine:3.20
sbx push -a ghcr.io/me/alpine                 # every local tag of the repository
sbx --format json push ghcr.io/me/alpine:3.20  # one Pushed line per tag
```

`push` sends the layers, the config and last the manifest, exactly as they were pulled or loaded, so the image keeps
its ID (the registry reports the same digest). A layer the registry already has is not sent again; one that came
from another repository of the same registry is mounted from there instead of uploaded. If anything fails before the
manifest, the tag on the registry is left as it was. Credentials are the same as for pulls below; `sbx login` with an
account that may push. `-q` prints only the pushed reference; `--format json` prints one `Pushed` line per tag, with
what happened to each layer (`exists`, `mounted`, `uploaded`). An image without a name can't be pushed: `tag` it
first. In `sbx tui`, `t` and `p` on the Images tab fill in `tag` and `push`.

Private images use the credentials you already have: `sbx` reads Docker's `config.json` (`$DOCKER_CONFIG` or
`~/.docker`), including its `credHelpers` and `credsStore` (`docker-credential-*`), then `REGISTRY_AUTH_FILE` and
the containers `auth.json` that podman and skopeo use. It has no credential store of its own, so `docker login` is
enough. `sbx login` and `sbx logout` write and remove the same entries:

```sh
echo "$GHCR_TOKEN" | sbx login -u me --password-stdin ghcr.io
sbx login                                      # Docker Hub; asks for the user name and password
sbx logout ghcr.io
```

`login` checks the credentials with the registry before it saves them. Without a credential helper they are saved
base64-encoded in `config.json`, like Docker does, readable only by you.

Mirrors, rewritten locations and blocked registries come from the containers
[`registries.conf`](https://github.com/containers/image/blob/main/docs/containers-registries.conf.5.md) that podman,
skopeo and buildah read: `$CONTAINERS_REGISTRIES_CONF`, else `~/.config/containers/registries.conf`, else
`/etc/containers/registries.conf`, plus `registries.conf.d/*.conf`. For example, to pull Docker Hub images through
a mirror first:

```toml
[[registry]]
location = "docker.io"
[[registry.mirror]]
location = "mirror.gcr.io"
```

Mirrors are tried in order, then the registry itself; the image keeps the name you asked for. `push` never uses a
mirror or a rewritten `location`: it goes to the registry the name points at, and refuses a `blocked` one. `prefix`, `blocked`,
`insecure` (falls back to plain HTTP; unlike podman, certificates are still checked, so add a self-signed CA to the
system trust store), `mirror-by-digest-only` and `pull-from-mirror` work as
the containers documentation describes. `unqualified-search-registries` and short-name aliases are not used: like
Docker, `sbx` reads a short name as a Docker Hub image. A mistake in the file stops pulls with an error naming it.

Signature checks follow the containers
[`policy.json`](https://github.com/containers/image/blob/main/docs/containers-policy.json.5.md)
(`~/.config/containers/policy.json`, else `/etc/containers/policy.json`; without either, everything is accepted, as
with Docker). `sbx` checks cosign signatures made with a key (`cosign sign --key`): with this policy, images under
`ghcr.io/my-org` must carry a signature by `cosign.pub`, and everything else is pulled as before:

```json
{
  "default": [{ "type": "insecureAcceptAnything" }],
  "transports": {
    "docker": {
      "ghcr.io/my-org": [{
        "type": "sigstoreSigned",
        "keyPath": "/home/me/cosign.pub",
        "signedIdentity": { "type": "matchRepository" }
      }]
    }
  }
}
```

The check runs before any layer is downloaded, and a `reject` before `sbx` contacts the registry at all. cosign
signs a repository without a tag, so a tag pull needs `matchRepository` (or `exactRepository`, `remapIdentity`), as
with podman. Requirements `sbx` can't check (`signedBy` GPG signatures, keyless `fulcio`/`pki` and `rekorPublicKey*`)
make the images they cover fail to pull, never pass. `sbx load` follows the `oci-archive` (tar) or `oci` (directory)
transport and refuses an archive whose policy asks for signatures. The policy is about what may come in, so `push`
doesn't consult it (neither does podman for the registry it pushes to).

The guest kernel comes with the executor. An image can bring its own instead, with the label
`io.github.toollyst.sandbox.kernel=/path/in/image`; other labels under `io.github.toollyst.sandbox.` are reserved
for `sbx` and an image can't set them.

## Build

`sbx build` makes an image from a Dockerfile, with buildx's flag names. The image goes into the same store as pulled
images, so `sbx run` can use it right away:

```sh
sbx build -t local/app:dev .                     # PATH/Dockerfile, the context is PATH
sbx build -f ci/Dockerfile --target test --build-arg VERSION=1.2 .
sbx build --network allow --allow registry.npmjs.org:443 -t local/web .  # RUN npm ci may reach npm
sbx build --progress plain .                     # stream RUN output as it comes
sbx build -q .                                   # print only the image ID
sbx --format json build .                        # one BuildEvent JSON line per step and output chunk
```

Each stage's `RUN` steps run in **one microVM**, started at the first `RUN` that isn't cached, with the layers built
so far as its root filesystem. Each `RUN` gets a new writable layer; its changes are copied out as the step's layer,
and anything the step left running is ended. `COPY`, `ADD` and `WORKDIR` layers are made on the host from the
context and the layers below, so a `FROM scratch` stage of copies, or a rebuild where every `RUN` is cached, starts
no VM at all. `--cpus` and `-m` size the build VM (1 vCPU and 512m by default); its writable disk is 16 GiB, sparse.
Stages are built one after another, never in parallel. A step's progress goes to stderr as `[2/7] RUN …`, then
`DONE 1.2s` or `CACHED` (`[name 2/7]` when there are several stages, `stage-N` for one without a name); when a `RUN` fails, the end of its
output is shown with the exit code.

The **network is closed** while building, unlike Docker. `--network allow` with `--allow HOST:PORT` opens
destinations for `RUN` steps exactly as for `sbx run`, through the proxy and transparent TCP. When a failed step
was refused a name, `sbx` lists it: `blocked: registry.npmjs.org — add --allow registry.npmjs.org:PORT`. The port is
left for you to fill in, because only the name was asked for.

The **context** is a local directory. Files that `.dockerignore` (or `FILE.dockerignore` next to `-f FILE`, which
wins, as in BuildKit) excludes don't exist for `COPY`. Paths can't leave the context: `../x` and symlinks are
resolved with the context as `/`, as Docker does. Sockets, FIFOs and devices in the context are skipped. A URL, a
tar on stdin and `-f -` are refused.

The **cache** keeps every step that makes a layer, keyed by the step, the layer below it, its build arguments and,
for `COPY`, the content it copies (names, modes, sizes, link targets and file contents; not modification times). A
rebuild with nothing changed gives the same image ID. `--no-cache` doesn't read the cache but still records what it
builds. Images built on the same base share its layer disks, and so do the layers the cache holds. `sbx system df`
shows the cache as `build cache`, and `sbx system prune` empties it (the layers it alone kept go in the same run).

What Docker does differently, and what isn't there yet, is in the build rows of
[Docker compatibility](docker-compatibility.md). In short: `ADD` copies a local file but refuses an archive Docker
would unpack; `ONBUILD` (in the Dockerfile or the base image), `RUN --mount`, heredocs and `--platform` other than the
host's are errors with the line number, never ignored; `HEALTHCHECK` is written to the image but `sbx` doesn't run
it. A built image has no tar layers yet, so `sbx save` and `sbx push` refuse it until the layer export step (`-o
type=docker|oci` comes with it).

## Resources

`--cpus` and `-m` set a sandbox's vCPUs and memory, and `--storage-opt size=20g` its writable disk (1 GiB by
default; the file is sparse, so the host only spends what the sandbox writes). A sandbox's output can't fill the host's memory or disk.

`--timeout 2h` stops a sandbox that long after each start (a plain number is seconds; `30m`, `1h30m` work too).
It gets its stop signal, then is powered off after its stop timeout, and `sbx inspect` shows `timeout_secs` and the
reason it stopped. `sbx start` starts the clock again. Which executor a sandbox runs on is covered in
[Executors](executors.md).

## Disk usage and cleanup

```sh
sbx system df                  # images, build cache, sandboxes, volumes and executors: count, in use, size, reclaimable
sbx system prune               # stopped sandboxes, the build cache, unused layers, leftovers (asks first)
sbx system prune -a -f         # also every image no sandbox uses; -f doesn't ask
sbx system prune --filter until=24h --filter label=ci=1
sbx system prune -a -f --volumes   # also every volume no sandbox points at
```

Sizes are what is actually on disk: a sandbox's writable disk counts what it wrote, and a layer several images use
counts once (the `SHARED` column). `prune` never touches running sandboxes, the images they use, or volumes unless
asked with `--volumes`, and only removes leftovers older than an hour (another `sbx` may still be writing
them). Without a terminal it needs `-f`. In `sbx tui`, `U` and `P` on the Images tab do the same.

## Scripting

- `--format json` prints only JSON on stdout, with errors as one JSON line on stderr. Commands that report something
  print a value or JSON Lines; commands whose output is a process's own bytes (`exec`, attached `run`) refuse it.
- `--jq EXPR` picks fields with a built-in jq, so you don't need jq installed: `sbx --format json ps --jq '.[].name'`.
- `ps`, `images` and `events` take `--filter KEY=VALUE` with Docker's rules. `-q` prints IDs only.
- `sbx events` streams what changes (sandboxes created, started, stopped and removed; commands run with `exec`, as
  `exec_start` with their argv and `exec_die` with their exit code; images pulled, built, prepared, untagged and deleted)
  with no daemon, and `--since 10m` replays the recent past first. The log lives in the data directory, readable
  only by you.
- `sbx logs` takes `-t`, `--since`, `--until` and `--format json`.

The JSON shapes and their JSON Schema are in [cli-output.md](../developers/cli-output.md); what counts as a breaking
change to them is in [COMPATIBILITY.md](../../COMPATIBILITY.md).

## Full-screen view

`sbx tui` shows sandboxes, images, volumes, events and executors on one screen. It is built on the same commands:
every key runs an `sbx` command and shows its command line, so anything you do there you can script.

The bottom line shows the keys you can use right now; `?` lists them all, by tab (so does `sbx tui --help`). Enter
opens the selected row's details, with the `sbx … --format json` command that prints the same fields. An empty tab
says what to do next, for example to install an executor before the first sandbox.
The header shows the `sbx` version, and a newer release in parentheses (red when it fixes a vulnerability or yours
was yanked); `V` runs `sbx upgrade`.
