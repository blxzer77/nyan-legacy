# Agent Note: Rust Coding Agent parity boundary

Status: proposed: RS0 is frozen and the RS1 Rust workspace scaffold is independently checked; the agent runtime and parity implementation do not exist yet.

## Problem

Nyan's current TypeScript CLI is a complete, published coding agent. A Rust rewrite must not accidentally become a reduced single-agent prototype. The existing CLI continues to serve users while the rewrite is built, and future product surfaces do not define this rewrite.

The rewrite also has a strong performance constraint. Rust should own the normal runtime, persistence, process lifecycle, CLI, and terminal-facing interaction. A compatibility dependency on Node or another runtime must not become a requirement for ordinary Coding Agent execution.

## Proposal

Build a complete Rust Coding Agent first. The Rust product is a standalone terminal coding agent with the current CLI's user-visible capabilities and stable machine interfaces:

- interactive terminal mode;
- print, JSONL event, and RPC modes;
- provider, model, credential, and authentication handling;
- tools and subprocess execution;
- sessions, continuation, branching or fork semantics where supported, export, and recovery;
- MCP, skills, prompt templates, settings, trust, and extension boundaries;
- compaction, cancellation, retry, and error outcomes.

The current TypeScript CLI remains the production and behavior reference until the Rust implementation passes the parity gate. The first Rust preview uses a distinct executable or release channel and must not overwrite the existing `nyan` installation or its state without an explicit cutover. The eventual cutover is a separate roadmap gate, after which the TypeScript implementation remains available as a compatibility or rescue path for a defined transition window.

The Rust implementation is organized as a Cargo workspace in this repository, separate from the TypeScript packages. The initial workspace is library-first: the CLI depends on Rust core crates and exposes a stable core/protocol boundary for later consumers. Project dashboards, multi-project orchestration, and other larger product surfaces are outside this track.

Rust owns the ordinary runtime. Built-in behavior and the fast path do not start Node. Existing TypeScript extensions or other compatibility surfaces may use an optional, isolated bridge if parity work proves that bridge necessary, but the bridge is an RS6 compatibility gate and is not part of the Rust core's ownership. No compatibility reduction is silently accepted: a missing surface is either implemented, explicitly reduced by an owner decision, or blocks the parity gate.

The parity evidence is black-box where possible. The same prompts, tool fixtures, provider fixtures, session cases, signals, and protocol requests run against the TypeScript and Rust CLIs; outputs are normalized only for unstable fields such as timestamps and generated identifiers. The contract records stdout, stderr, exit codes, event discriminants, state transitions, persistence, cancellation, and recovery, not just final assistant text.

The Rust line is divided into these stages:

- **RS0 — surface inventory and parity contract:** enumerate the current CLI and freeze the compatibility matrix.
- **RS1 — Cargo workspace and core boundaries:** establish Rust crates, errors, events, storage ports, provider/tool ports, and independent checks.
- **RS2 — Agent runtime parity:** implement model turns, tool loops, streaming, abort, retry, subprocess ownership, and stable outcomes with fake providers.
- **RS3 — session and persistence parity:** implement versioned durable state, locks, continuation, fork/clone behavior, export, restart recovery, and legacy-session reading or migration.
- **RS4 — CLI and protocol parity:** implement interactive, print, JSONL, and RPC surfaces with stable flags, exit codes, stdout/stderr ownership, and signal behavior.
- **RS5 — provider and resource parity:** implement providers, models, auth, tools, MCP, skills, prompt templates, settings, trust, and built-in resources.
- **RS6 — extension compatibility:** decide and verify the native Rust extension boundary and any optional out-of-process compatibility bridge.
- **RS7 — preview and parity gate:** run differential, recovery, terminal, multi-platform, startup, memory, and performance checks while publishing the Rust preview separately.
- **RS8 — cutover:** switch the default executable only after the owner accepts the complete parity and rollback evidence.

## Alternatives considered

### Rewrite only the Agent loop

Rejected. It would leave CLI modes, sessions, auth, extensions, MCP, recovery, and published interfaces as unowned compatibility gaps. A Rust binary that can answer one prompt is not a replacement for the current Coding Agent.

### Build a larger product surface before CLI parity

Rejected for this track. It would couple the core rewrite to a new product surface and make it difficult to prove that the terminal Coding Agent itself is complete.

### Keep the TypeScript runtime as the required Rust backend

Rejected as the normal architecture. It would preserve a Node dependency in the hot path, weaken the performance and memory goals, and make the Rust executable a wrapper rather than a rewrite. An optional compatibility bridge may still be used for a bounded extension surface.

### Add Rust code directly into the existing TypeScript package layers

Rejected. The Rust workspace needs native crate boundaries and Rust-specific tests. It may share black-box compatibility fixtures and repository governance, but it must not pretend that TypeScript module layers are Rust crate boundaries.

### Maintain a long-lived Rust branch and merge it at the end

Rejected. The TypeScript CLI changes continuously, so a long-lived branch would hide parity drift until the most expensive point. The Rust stages should merge incrementally, remain independently buildable, and use the TypeScript implementation as a continuously tested reference.

## Consequences

- The TypeScript CLI remains a supported implementation during the rewrite; its user-visible contracts cannot be changed casually to simplify Rust.
- The current CLI surface, session formats, JSON/RPC protocols, extension API, and resource behavior become parity inputs rather than automatic Rust designs.
- The Rust workspace adds a second build system and needs dedicated formatting, lint, test, cross-platform, and release checks. The first machine-readable reference inventory is [the CLI surface matrix](../../../compat/coding-agent/cli-surface.json); it is a starting RS0 artifact, not a completed parity claim.
- The first Rust release is a Coding Agent release, not an expanded orchestration product. Larger project orchestration remains outside the initial Rust CLI gate.
- Rust's process owner is explicit: an interactive CLI process owns its active Agent runs, and closing the process cancels them. A forced termination records an unknown or interrupted outcome rather than silently replaying side effects.
- Compatibility bridges, if needed, must be isolated behind typed protocols and must not leak runtime-specific types into the Rust core.
- A successful parity gate requires more than equal final text: it requires evidence for persistence, cancellation, recovery, errors, protocols, and user-visible CLI behavior.

## Verification

This note records the product boundary and the proposed sequence. The Rust workspace scaffold now exists under `rust/`, with no completed agent runtime or parity claim yet. RS0 started with `compat/coding-agent/cli-surface.json`, verified against `origin/main` commit `24424b1298434ec7194444f41d51386b8496b12e`; RS1 created the library-first crate boundaries and standard-library-only scaffold. The inventory now covers the four runtime modes, 13 top-level command surfaces, 44 common options, command-specific auth/package/MCP/experimental options, dynamic extension flags, 23 session event types, RPC-only extension errors, 33 RPC commands, lifecycle and signal rules, process/tool environment variables, persistence, and mode/session constraints. The provider credential source `packages/ai/src/env-api-keys.ts` is recorded in the matrix, including ambient AWS and Vertex credential paths. The extension documentation, durable reductions, RPC UI behavior, exit codes, durable session error codes, model error codes, and RPC failure framing have also been reviewed and recorded. RS0's parity matrix is now frozen at the verified commit above; future source changes require a deliberate matrix update and parity review before they are treated as Rust requirements. RS1's independent gate passes formatting, Cargo checking, Clippy with warnings denied, workspace tests, and the scaffold CLI's version/help behavior. RS2 has started with an executor-neutral runtime crate: provider streaming events become assistant message lifecycle events, sequential and parallel tool calls produce model-visible tool results, retryable provider admission failures emit retry events, tool partial results are delivered through the shared execution context while the tool is running, cancellation produces an explicit aborted outcome when no external effect started, and an uncertain effect produces a distinct unknown-outcome terminal state. The tools crate also owns a process-group future that captures stdout/stderr and kills the whole platform-native group on cancellation through the pinned `command-group` wrapper. Fake-provider and subprocess tests cover a tool loop, streaming updates, admission retry, a retryable stream failure before output, exhausted retry failure, stable unsupported/stream/turn-limit errors, pre-run cancellation, tool-triggered cancellation, output capture, and process-group cancellation. The Linux host also cross-checks the `x86_64-pc-windows-gnu` target with Cargo check and Clippy, and `.github/workflows/rust.yml` runs the full Rust checks on both Linux and Windows runners. Native Windows Job Object execution remains CI-only until that workflow runs. RS3 groundwork now includes an independent Rust-native file session store with a versioned header, cross-platform lock lease, ephemeral sessions, append-only records with sequence numbers, replay, a provider-neutral message codec, a NativeSession reopen/transcript path, session-id selection, fork/clone message-prefix operations, and invalid/corrupt/unsupported header diagnostics; it does not yet replace or migrate the durable SQLite/legacy JSONL stores. This is still an initial RS2 slice, not complete parity.
