## What and why

<!-- One change. What it does, and the problem it solves. -->

## Checks

- [ ] `cargo test --workspace --all-features`, `cargo clippy ... -D warnings`, `cargo fmt --all --check`, `cargo xtask check-deps`
- [ ] Tests that fail without this change
- [ ] If it touches the run or exec path: `cargo xtask bench` before and after
- [ ] If it changes CLI behavior vs Docker: listed in `behavior.rs`
- [ ] Every commit is signed off (`git commit -s`, DCO)
- [ ] If it changes a public contract (CLI, JSON output, MCP, protocols, index, on-disk layout): an issue and a roadmap note
- [ ] If users or scripts will notice it: a line under `## Unreleased` in `CHANGELOG.md` (Breaking, Security, Added or Fixed)
