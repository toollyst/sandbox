# Use it like Docker

`sbx` takes the commands and flags you know from `docker`. Where it differs, it's on purpose, and every difference is
[listed in one place](../../crates/app/cli/src/behavior.rs).

- [Run and manage sandboxes](#run-and-manage-sandboxes)
- [Files](#files)
- [Network](#network)
- [Images](#images)
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
sbx cp dev:/etc/alpine-release .
sbx logs -f dev
sbx logs -t --since 10m dev                          # with timestamps, the last 10 minutes only
sbx stop dev && sbx rm dev

sbx rm -f $(sbx ps -aq)                              # clean up everything
```

The lifecycle commands are `create`, `start [-a] [-i]`, `stop [-s SIG] [-t SECS]`, `restart`, `kill [-s SIG]`,
`wait`, `attach` (detach with Ctrl-P Ctrl-Q) and `rm [-f]`. A stopped sandbox keeps its disk, so `start` picks up
where it left off. `-e`, `--env-file`, `-w`, `-u`, `--name`, `--label` and `--entrypoint` work as in Docker.

Exit codes follow Docker: 125 when `sbx` fails, 126 when the command can't run, 127 when it isn't found, and the
command's own code otherwise.

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

## Network

The VM never gets a network interface. With `--network allow`, an HTTP proxy inside the guest hands each connection
to the host over vsock, and the host checks it against your list:

```sh
sbx run --rm --network allow \
    --allow pypi.org:443 --allow files.pythonhosted.org:443 \
    python:3.12-alpine pip install requests
```

A rule can be `host:port`, `*.example.com:443`, `10.0.0.5:*` or `[::1]:80`. A name that resolves to a loopback or
private address is refused unless the rule names that IP, or `localhost`, explicitly. Without `--network allow`, name
lookups fail at once instead of hanging.

## Images

`pull` (with a progress bar), `prepare` (build the disk image ahead of time), `images [-q]` and `rmi [-f]`. `run`
pulls and prepares on first use (`--pull=missing|always|never`). Images come from Docker Hub, GHCR or any OCI
registry, unmodified. Each is converted once into a read-only EROFS disk image and cached, so after the first run,
starting a sandbox is just booting a VM.

The guest kernel comes with the executor. An image can bring its own instead, with the label
`io.github.toollyst.sandbox.kernel=/path/in/image`; other labels under `io.github.toollyst.sandbox.` are reserved
for `sbx` and an image can't set them.

## Resources

`--cpus` and `-m` set a sandbox's vCPUs and memory, and `--storage-opt size=20g` its writable disk (1 GiB by
default; the file is sparse, so the host only spends what the sandbox writes). A sandbox's output can't fill the host's memory or disk. Which executor a sandbox runs on is covered in
[Executors](executors.md).

## Disk usage and cleanup

```sh
sbx system df                  # images, sandboxes and executors: count, in use, size, reclaimable
sbx system prune               # stopped sandboxes, unused layers, leftovers of interrupted pulls (asks first)
sbx system prune -a -f         # also every image no sandbox uses; -f doesn't ask
sbx system prune --filter until=24h --filter label=ci=1
```

Sizes are what is actually on disk: a sandbox's writable disk counts what it wrote. `prune` never touches running
sandboxes or the images they use, and only removes leftovers older than an hour (another `sbx` may still be writing
them). Without a terminal it needs `-f`. In `sbx tui`, `U` and `P` on the Images tab do the same.

## Scripting

- `--format json` prints only JSON on stdout, with errors as one JSON line on stderr. Commands that report something
  print a value or JSON Lines; commands whose output is a process's own bytes (`exec`, attached `run`) refuse it.
- `--jq EXPR` picks fields with a built-in jq, so you don't need jq installed: `sbx --format json ps --jq '.[].name'`.
- `ps`, `images` and `events` take `--filter KEY=VALUE` with Docker's rules. `-q` prints IDs only.
- `sbx events` streams what changes (sandboxes created, started, stopped and removed; images pulled, prepared,
  untagged and deleted) with no daemon, and `--since 10m` replays the recent past first.
- `sbx logs` takes `-t`, `--since`, `--until` and `--format json`.

The JSON shapes and their JSON Schema are in [cli-output.md](../developers/cli-output.md); what counts as a breaking
change to them is in [COMPATIBILITY.md](../../COMPATIBILITY.md).

## Full-screen view

`sbx tui` shows sandboxes, images, events and executors on one screen. It is built on the same commands: every key
runs an `sbx` command and shows its command line, so anything you do there you can script.
