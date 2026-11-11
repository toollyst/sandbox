# sbx user guide

For people who use `sbx`. How the code works and how to change it is in the [developer docs](../developers/).

| Page | What's in it |
| --- | --- |
| [Install](install.md) | macOS, Linux and Windows, installer options and signature checks, building from source, upgrading and uninstalling |
| [Use it like Docker](usage.md) | Running and managing sandboxes, files, the network allow list, images, scripting with JSON, `sbx tui` |
| [AI agents (MCP)](mcp.md) | `sbx mcp`, its tools, and what an agent can and can't do |
| [Executors](executors.md) | Executors, adapters and VMM packages, versions, bringing your own VMM, repositories and publishing your own |

Also at the top of the repository:

- [SECURITY.md](../../SECURITY.md): reporting vulnerabilities, verifying what you install, and the threat model.
- [COMPATIBILITY.md](../../COMPATIBILITY.md): what may change between releases, and what counts as breaking.
- [CHANGELOG.md](../../CHANGELOG.md): what changed in each release.

## Use it as a library

The CLI and the MCP server are thin layers over two plain Rust crates: `sbx-manage` (images and cache) and
`sbx-runtime` (sandbox lifecycle). Neither assumes a CLI, and they can share state directories or keep their own.
They are not on crates.io yet and their API will change before 1.0; for now, depend on them through git. The
[developer docs](../developers/crates.md) describe every crate. Bindings for other languages are planned.
