# Rust Coding Agent

This Cargo workspace is the native implementation track for Nyan's complete terminal Coding Agent. It is not the GUI and it is not a usable replacement yet.

The workspace is library-first:

- `nyan-coding-agent-core`: runtime-neutral domain types, cancellation, messages, and stable errors;
- `nyan-coding-agent-session`: session identity and persistence boundaries;
- `nyan-coding-agent-provider`: model-provider boundary;
- `nyan-coding-agent-tools`: tool registration and execution boundary;
- `nyan-coding-agent-runtime`: executor-neutral turn, streaming, retry, cancellation, and tool-loop runtime;
- `nyan-coding-agent-protocol`: JSONL/RPC wire-name and event contracts;
- `nyan-coding-agent-cli`: the first native executable boundary.

The ordinary Rust path has no Node or external application runtime dependency. The current RS2 slice includes executor-neutral turn/tool execution (sequential and parallel), live tool updates, retry admission, cancellation, explicit unknown-effect outcomes, and platform-native process-group ownership through a pinned Rust wrapper. Persistence, provider adapters, and protocol serialization remain later roadmap stages.

## Checks

Run from this directory:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo run -p nyan-coding-agent -- --version
```

The repository workflow runs the same Rust checks on both `ubuntu-latest` and
`windows-latest`. From a Linux development machine, the Windows target can be
checked without executing Windows binaries:

```bash
rustup target add x86_64-pc-windows-gnu
cargo check --workspace --all-targets --target x86_64-pc-windows-gnu --locked
cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings
```

Windows-native process-group tests still require the Windows CI runner or a
Windows machine; the Linux host cannot execute the Windows Job Object path.
