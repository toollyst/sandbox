# Security

`sbx` exists to run code you don't trust. This page says what it defends against, how, and what it does not
cover yet. If you find a way across one of the boundaries below, please report it privately.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting: **Security → Report a vulnerability** on this repository. Please
don't open a public issue. Include the output of `sbx info` (version, host, hypervisor and executor) and the smallest
reproduction you have.

`sbx` is pre-release. Fixes go to the latest release and `main` only.

## Threat model

**Assumed hostile:** everything inside a sandbox. That includes root in the guest, the image's contents, and a
guest kernel that has been taken over. The image itself (its layers and config, as pulled from a registry) is
also untrusted input to the host.

**Trusted:** the host, the user running `sbx`, the `sbx` binaries, and the executor you install (adapter, VMM and
default kernel).

### Boundaries, and how each is held

| Boundary | What holds it |
| --- | --- |
| Guest → host CPU and memory | Hardware virtualization (KVM on Linux, Hypervisor.framework on macOS) through the VMM. Each sandbox has its own kernel. On Linux the VMM is upstream Firecracker with its default seccomp filters on (`sbx` never passes `--no-seccomp`). On macOS it is [our Hypervisor.framework port](https://github.com/toollyst/firecracker); macOS has no seccomp, so that layer of Firecracker's defense in depth does not exist there, and the port's device code has not had the review upstream has had. |
| Guest → host network | The VM has **no network device**. With `--network allow`, a proxy inside the guest hands each connection to the host over vsock. The host checks `host:port` against your list, resolves names itself, and refuses names that resolve to anything but internet addresses (loopback, private, link-local, CGNAT, reserved, documentation, IPv6 unique-local and site-local, and IPv4 inside NAT64 or 6to4 addresses judged by that IPv4) unless the rule names that IP or `localhost`. This blocks DNS rebinding onto your LAN or cloud metadata endpoints. A sandbox can hold at most 256 connections open at once. |
| Guest → host messages | Host code that reads guest messages (exec I/O, file copy, egress) treats them as hostile, and the repository has tests that play a lying guest. Frames are capped at 16 MiB. In `cp` from the guest, the host picks the name of what it writes from the path you asked for (never from the guest's answer), every path must be a safe relative path under it, and a symlink in the middle of a path is an error, so a guest can't write outside the destination you chose. Setuid, setgid and sticky bits are dropped. Output the host hasn't consumed yet is capped (4 MiB per process; the guest waits), and a detached sandbox's `main.log` stops at 256 MiB. |
| Guest → host files | The image's root filesystem is a read-only EROFS disk shared by sandboxes of that image. Writes go to a per-sandbox disk. Host directories are visible to a sandbox only if you pass `-v`/`--mount`, and the Firecracker executor doesn't support live mounts at all (`type=copy` copies at start). |
| Image → host (during pull and prepare) | Layers are verified against their SHA-256 digests, and a download stops as soon as it is larger than the manifest says. They are never unpacked onto the host filesystem by path: file contents are stored under generated names and merged into a single EROFS image file. Symlinks are resolved inside the image only (at most 40 hops). Only the registry host you named may be on a private or local address; token endpoints and redirects the registry hands out must be HTTPS and are resolved to internet addresses only, so a hostile registry can't make `sbx` fetch from your LAN or a metadata endpoint. Stalled reads time out after 60 s. |
| Executor bundles → host | Bundles are checked against the size and SHA-256 in the repository index, and fetched over HTTPS (plain HTTP is accepted only from loopback; `file://` from local disk). |
| AI agents (`sbx mcp`) | The MCP server adds no capabilities beyond the CLI. It never mounts host folders, keeps the network off unless a call passes `allow`, and builds arguments so tool input can't become other `sbx` options. Its tools only see sandboxes created through MCP, so an agent can't reach a sandbox you started with `-v` (`sbx mcp --all-sandboxes` widens this). `read_file` reads into memory and never follows a link on the host. Cancelled calls and a vanished client power off and remove what they started. |

### Not covered yet

These are known gaps, in rough order of importance. Some are on the [roadmap](docs/roadmap.md).

- **No jailer.** The VMM runs as your user, without Firecracker's jailer (chroot, namespaces, cgroups). An escape
  from the VM lands as your user on the host.
- **Repository indexes are not signed.** SHA-256 checks protect integrity against the index, and HTTPS protects the
  transfer, but whoever controls the repository host controls what you install.
- **Data you allow out can leave.** The allow list decides *where* a sandbox may connect, not *what* it sends there.
- **Resource limits are coarse.** vCPU count, memory and scratch disk size are fixed per sandbox. There are no CPU
  quotas, I/O limits or caps on how much disk an image pull and prepare may use, and `sbx mcp` does not cap how
  many sandboxes, vCPUs or how much memory an agent asks for.
- **The macOS VMM is a port.** See the first boundary above: no seccomp on macOS, and the Hypervisor.framework
  port is young.
- **Side channels** between VMs on the same host (CPU caches, timing) are out of scope.
- **The guest init and its protocol have not been audited by a third party.** The decoders are tested with 200,000 random and mutated messages on every run, but there is no coverage-guided fuzzing yet.
