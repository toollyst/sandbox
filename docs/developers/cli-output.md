# CLI JSON output

<!-- Generated from crates/app/cli/src/output.rs. Do not edit: run `SBX_BLESS=1 cargo test -p sbx-cli --lib output`. -->

With `--format json`, sbx prints only JSON on stdout. This is a public contract: scripts, agents and `sbx tui` rely on it. sbx is alpha, so it may still change, but only together with this file.

- **value**: one JSON value (pretty-printed).
- **lines**: one JSON object per line (JSON Lines), one per argument. If one argument fails, the others still run. Streams (`events`, `logs`) print one per item.
- **empty**: nothing on success.
- **not supported**: sbx refuses `--format json` and exits with an error.
- `-q` (`ps`, `images`) wins over `--format`: one ID per line.
- `--jq EXPR` (only with `--format json`, like gh) runs a jq expression on each value or line and prints each result on its own line, strings without quotes. A bad expression fails before the command does anything. jq is built in (jaq).
- Errors go to stderr as one `ErrorLine` per line. Other stderr lines are for humans.
- Field names are snake_case, times are RFC 3339 (UTC), missing values are `null`.

The JSON Schema (draft 2020-12) of every type is in [cli-output.schema.json](cli-output.schema.json).

| Command | Output |
|---|---|
| `run` | value: `SandboxRef` when detached; not supported when attached |
| `create` | value: `SandboxRef` |
| `start` | value: `SandboxRef` when detached; not supported when attached |
| `stop` | lines: `Done` |
| `restart` | lines: `Done` |
| `kill` | lines: `Done` |
| `wait` | lines: `Exited` |
| `rm` | lines: `Done` |
| `ps` | value: `[Sandbox]` |
| `inspect` | value: `Sandbox` |
| `exec` | not supported: its output is the process's own bytes |
| `attach` | not supported: its output is the process's own bytes |
| `logs` | lines: `LogLine` |
| `cp` | empty |
| `pull` | value: `Pulled` |
| `prepare` | value: `Prepared` |
| `images` | value: `[Image]` |
| `rmi` | lines: `Removed` |
| `tui` | not supported: it is an interactive screen |
| `events` | lines: `Event` |
| `mcp` | not supported: it speaks the Model Context Protocol (JSON-RPC) on stdio |
| `executor ls` | value: `[InstalledExecutor]` |
| `executor install` | value: `ExecutorRef` |
| `executor search` | value: `[AvailableExecutor]` |
| `executor use` | value: `ExecutorRef` |
| `executor current` | value: `ChosenExecutor` |
| `executor which` | value: `ChosenExecutor` |
| `executor rm` | value: `ExecutorRef` |
| `executor prune` | value: `[Pruned]` |
| `executor adapter ls` | value: `[InstalledAdapter]` |
| `executor adapter install` | value: `ExecutorRef` |
| `executor adapter search` | value: `[AvailableExecutor]` |
| `executor adapter rm` | value: `ExecutorRef` |
| `executor repo ls` | value: `[Repository]` |
| `executor repo add` | value: `Repository` |
| `executor repo rm` | value: `Repository` |
| `info` | value: `Info` |
| `completion` | not supported: its output is a shell script |
