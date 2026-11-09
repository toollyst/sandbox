## What and why

<!-- One change. What it does, and the problem it solves. -->

## Checks

- [ ] `cargo test --workspace --all-features`, `cargo clippy ... -D warnings`, `cargo fmt --all --check`, `cargo xtask check-deps`
- [ ] Tests that fail without this change
- [ ] If it touches the run or exec path: `cargo xtask bench` before and after
- [ ] If it changes CLI behavior vs Docker: listed in `behavior.rs`
