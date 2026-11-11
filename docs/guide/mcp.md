# Give your AI agent a computer

`sbx mcp` serves sandboxes over the [Model Context Protocol](https://modelcontextprotocol.io). Your agent gets a whole
Linux machine it can install packages on, break and throw away, and your laptop stays out of reach.

```sh
claude mcp add sbx -- sbx mcp          # Claude Code
```

Other clients take the same command in their configuration:

```json
{ "mcpServers": { "sbx": { "command": "sbx", "args": ["mcp"] } } }
```

## Tools

| Tool | What it does |
| --- | --- |
| `run` | Run one command in a fresh microVM and return the exit code, stdout and stderr. The VM is deleted afterwards |
| `create` | Start a long-lived sandbox, optionally named. It outlives the MCP server |
| `exec` | Run a command in a running sandbox |
| `write_file` / `read_file` | Move files in and out, as text or base64. Parent directories are created |
| `list` / `remove` | See and clean up sandboxes |

## What an agent can and can't do

The MCP server adds no policy of its own. Tool arguments go through the same parser and rules as the CLI:

- The network stays off unless a call passes `allow: ["host:port", …]`.
- Host folders are never mounted, and tools only see sandboxes created through MCP, not the ones you started with
  the CLI (`sbx mcp --all-sandboxes` changes that).
- Arguments can't be turned into other `sbx` options.
- `read_file` reads into memory and never follows a link on the host.
- Agents get at most 8 sandboxes at once per MCP server, each with at most 4 vCPUs and 4 GiB of memory. MCP
  sandboxes that other servers on your data directory already made count too, but two servers creating at the same
  moment can each take the last place. Change these with `--max-sandboxes`, `--max-cpus` and `--max-memory`, for
  example `sbx mcp --max-sandboxes 2 --max-memory 1g`.

Every command has a timeout (300 s by default). When it runs out or the client cancels the call, `run` powers the VM
off and `exec` kills the process; a client that goes away has what it started powered off and removed. Output keeps
the last 256 KiB of each stream, where errors usually are. The first pull of a large image reports progress
(`notifications/progress`), so clients don't give up waiting.

The threat model for agents is in [SECURITY.md](../../SECURITY.md#threat-model).
