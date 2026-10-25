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

Status of the Rust track, kept as facts with the evidence behind each and the gaps stated next to them. Nothing here is a parity claim: the Rust executable is a separate preview (`nyan-coding-agent`) and the TypeScript `nyan` remains the product.

### RS0: parity contract

`compat/coding-agent/cli-surface.json`, verified against `origin/main` `24424b1298434ec7194444f41d51386b8496b12e`, is frozen: four modes, 13 top-level command surfaces, 44 common options, command-specific options, dynamic extension flags, 23 session event types, 33 RPC commands, RPC extension UI, persistence, environment, extension behavior, error contracts, and exit codes. Later source changes need a deliberate matrix update.

### RS1: workspace

Library-first crates under `rust/`: core, session, provider, provider-http, tools, runtime, protocol, cli. Gate: `cargo fmt --check`, `cargo check`, `cargo clippy -D warnings`, `cargo test` (Linux), plus `cargo check` and `cargo clippy` for `x86_64-pc-windows-gnu`. `.github/workflows/rust.yml` runs the Rust checks on Linux and Windows runners; **it has never run on GitHub**, so every Windows-native behavior below is unverified.

### RS2: single-agent runtime

Evidence: fake-provider tests. Covered: streaming into assistant message events with content-block updates, sequential and parallel tool loops, live tool updates, admission and stream retry before output, retry exhaustion, cancellation, stable error codes (`unsupported`, `stream`, `turn_limit`, ...), and a distinct unknown-outcome terminal state for an effect that started but cannot be proven finished. Subprocesses run in a platform-native process group (Unix process group, Windows Job Object through the pinned `command-group` wrapper), with timeout, streaming output, and a 500 ms drain grace so a background grandchild cannot hold the result hostage. A finished or dropped process future ends only its own group; an end-to-end test caught that it once cancelled the whole run.

### RS3: sessions

An independent Rust-native format (not the durable SQLite or legacy JSONL stores, which are neither read nor migrated): versioned header, cross-platform lock lease, append-only journal with sequence numbers and corruption detection, message codec version 2, reopen with transcript replay, session-id listing, fork and clone by message prefix, run-start and run-finish markers with incomplete-run detection, and `run_persistent`, which refuses a session whose previous run never finished. **No CLI path uses sessions yet**, and no storage directory has been chosen.

### RS4: CLI and protocol

- Argument parsing is a branch-for-branch port of `cli/args.ts`; diagnostics follow `main.ts` (`Error:` and `Warning:` on stderr, exit 1 on any error).
- App-mode resolution is ported from `resolveAppMode`.
- `--print` and `--mode json` run end to end for `--provider openai` (or `--model openai/<id>`) with `OPENAI_API_KEY` or `--api-key`. Print mode follows `modes/durable/print.ts`; JSON mode writes the session header then every event and keeps stdout JSONL-only. Evidence is the same `run` function the executable calls, driven against a loopback endpoint, including model-driven `write` and `bash` tool calls that change the workspace and whose results reach the next request.
- JSONL framing is LF-only; U+2028 and U+2029 stay inside strings.
- Wire records exist for `agent_start`, `turn_start`, `message_start/update/end`, `tool_execution_start/update/end`, `turn_end`, `agent_end`, `agent_settled`, the retry events, and the session header. Updates are delta-only.
- Missing from the wire: `usage.cost` (no pricing), assistant `api`, `thinking` and `image` content blocks, `toolcall_delta`, `tool_execution_end.durationMs`, `queue_update`, `entry_appended`, `session_info_changed`, `thinking_level_changed`, and the compaction events.
- The system prompt is built from the TypeScript sections (preamble, tools, rules, optional addendum, project context, cwd) with the same wording and tag wrapping, and is the first message of every request. `--system-prompt` and `--append-system-prompt` take literal text or a file path, `--no-context-files` drops the instruction files, and context files load from the agent directory (`NYAN_CODING_AGENT_DIR`, else `~/.nyan/agent`) then every ancestor directory outermost first, with `AGENTS.override.md` beating `AGENTS.md` beating `CLAUDE.md` in one directory. Differences: no documentation section (the preview ships no docs), no skills section, no `NYAN_*` guideline for bash, and no linked-worktree shadowing of context files.
- Initial prompt: piped stdin (trimmed; skipped on a terminal), `@file` blocks, and the first positional message are concatenated with no separator, as the TypeScript CLI does; later positional messages are further turns. A missing file exits 1 before any request or session; empty files are skipped; image files are refused with a message because the runtime has no image content blocks. The macOS filename variants of `resolveReadPath` are not ported, and stdin is read after argument validation rather than before it.
- Not implemented: RPC mode and the interactive UI (both exit 2), extension flags (rejected with an error), the subcommands (`install`, `auth`, `mcp`, ...), `--help` text parity, and signal handling.
- Sessions: every run is saved in the Rust-native format under `<agent dir>/sessions/<working-directory hash>/` (`--session-dir` moves the root) unless `--no-session`. `--continue` reopens the directory's newest session, `--session-id` opens or creates that id, `--session` takes an exact id or a unique prefix, and `--name` stores a display name. The first message of a session is its system prompt, and a changed prompt replaces it. A session whose previous run never finished is refused with exit 1 (`session_incomplete_run`) rather than continued, and a session locked by another process is refused before any request. The store neither reads nor writes the TypeScript durable store or the legacy JSONL tree, so a session started in one CLI cannot be resumed in the other. Not implemented: `--resume` (needs a picker) and `--fork` are refused with a message; the JSON header `id` is the session id but records are not yet mirrored as `entry_appended` events; there is no session listing command, no export, and no compaction.

### RS5: providers and tools

Decision #47: `reqwest` with rustls on `tokio`, tokio confined to the network crate and the executable.

- `nyan-coding-agent-provider-http`: an incremental SSE decoder (chunk boundaries may fall anywhere) and an OpenAI-compatible chat-completions streaming provider. HTTP statuses map to stable kinds (401 and 403 authentication, 404 model, 408/409/425/429/5xx retryable, other 4xx fatal); tool calls are assembled across deltas; usage and a `length` finish are reported; a stream that ends without a terminal event is a retryable failure; cancellation takes effect within 50 ms; a silent stream times out. Evidence is a loopback server through the public `ModelProvider` entry point, with mutation checks on the 401 mapping and the cancellation check.
- Tool definitions carry a JSON Schema that reaches the provider request.
- Built-in tools `read`, `bash`, `edit`, `write` (the TypeScript default set and order), `grep`, `find`, and `ls` are ported with the TypeScript schemas, truncation limits (2000 lines or 50 KB), continuation notices, and error wording.
- Gaps: `read` refuses images instead of attaching them; `edit` matches exactly only (no fuzzy fallback for smart quotes, Unicode dashes and spaces, or trailing whitespace) and reports no diff details; `ls` sorts by lowercased byte order rather than ICU collation; macOS filename variants are not ported (Linux and Windows are the targets); `grep` and `find` search in process with the libraries ripgrep and fd are built on (`ignore`, `regex`, `globset`), so no external binary is needed, but they sort their results and skip `.git` (ripgrep with `--hidden` would search it; `fd` is believed to skip it, which is unverified), and a long grep line is cut by Unicode scalar values rather than UTF-16 units; `--tools`, `--exclude-tools`, `--no-tools`, and `--no-builtin-tools` choose the active set with the TypeScript `+name`/`-name` and `*` semantics, and an unknown name is an error because the preview has no extension or MCP tools; `bash` adds no `NYAN_*` variables, has no configurable shell path, and reports cancellation as an unknown outcome instead of "Command aborted"; file tools do not serialize concurrent mutations of one path.
- `OpenAiResponsesProvider` speaks `openai-responses` (the wire API the CLIProxyAPI catalog uses, and OpenAI, xAI, and Meta): stateless `store: false` requests to `{base}/responses`, the system prompt as a `developer` message (`system` with `with_system_role`), history as `message`/`function_call`/`function_call_output` items, text and refusal deltas, function calls assembled from argument deltas (found by `output_index`, else by `item_id`), and `response.completed`/`incomplete`/`failed` and `error` mapped to stop reasons and stable errors. A call whose `output_item.done` never arrived ends the response as an error instead of reaching the agent. Both OpenAI providers now share one transport (`transport.rs`), and `Usage.input` excludes cached tokens as the TypeScript providers count it (the chat provider counted them twice before this change). Gaps: no reasoning effort or summaries, no encrypted-reasoning replay (a reasoning item in a reply is skipped, so a reasoning model loses its earlier reasoning), no prompt-cache key, no image input, no custom/grammar tools, no `service_tier`, and the request carries no `max_output_tokens`. Evidence: 9 loopback tests through `ModelProvider`; seven mutations (unfinished call allowed, `max_output_tokens` read as stop, `item_id` fallback removed, `store: true`, tool output item renamed, reasoning item treated as a call, `error` event ignored) each fail a named test. It is not wired into the CLI yet: there is no CPA catalog, login, or credential store.
- Login and models: `nyan login [provider]` asks for the `CLIProxyAPI` base URL (default `http://127.0.0.1:8317`, any pasted catalog or inference URL is reduced to the root) and key (read without echo on a terminal, checked in a real PTY), proves them by fetching `{root}/v1/models?client_version=pi`, stores them, then lists the catalog and saves the chosen model as `defaultProvider`/`defaultModel` in `settings.json`; a rejected key or invalid URL asks again, and a cancelled login stores nothing. `nyan logout`, `--list-models [search]`, and the run path use the stored login: key from `--api-key`, then `CLIPROXYAPI_API_KEY`, then the store; base URL from `CLIPROXYAPI_BASE_URL`, then the one stored with the key. There is no default model: `--model` wins, then the saved default, otherwise the error says to log in or pick one. Credentials live in `auth.json` as `{"type":"api_key","key":...,"env":{"CLIPROXYAPI_BASE_URL":...}}` (mode 0600, written atomically under a lock, other providers' entries preserved, a malformed file refused). Gaps: only `cliproxyapi` can log in (`openai` still takes `OPENAI_API_KEY`, and still uses chat completions where the TypeScript `openai` provider uses Responses); the old `cliproxyapi.json` config file and the oauth-shaped login of the TypeScript extension are not read; no per-model overrides, pricing, thinking levels, pins, or on-disk catalog cache (`--list-models` fetches live); no `--model` pattern or `provider/id:thinking` syntax; no interactive picker or `/login` (the TUI does not exist); the Windows secret prompt is untested; `settings.json` writes are not locked across processes. Evidence: 8 CLI tests against a proxy stub (key and bearer checked on every route), 8 auth-store tests, 7 catalog tests, 4 settings tests; fifteen mutations across login, precedence, default selection, logout, search, locking, file mode, and validation each fail a named test.
- Wire protocols (all through `ModelProvider`, each with loopback tests and mutation checks): `openai-completions`, `openai-responses` (also Azure: `api-key` header, `api-version`, deployment-name map, resource-URL normalization), `anthropic-messages` (`x-api-key`, `anthropic-version`, content blocks, streamed tool JSON, cache and stop-reason usage), `google-generative-ai` and `google-vertex` (`contents`/`functionDeclarations`, key in `x-goog-api-key` never in the URL, thought parts hidden, Vertex API key or bearer), `mistral-conversations` (9-character tool-call ids, cached-token usage), and `bedrock-converse-stream` (SigV4 over `host`, `x-amz-date`, content type and session token, checked against the AWS `get-vanilla` vector and an independent Python implementation; binary event-stream framing with prelude and message CRC checks; usage that arrives after `messageStop`; exception frames mapped to retryable or fatal; consecutive tool results merged into one user message). A tool call whose end marker never arrived ends the response as an error in every protocol, never as a tool use.
- CLI provider resolution: a generated table (`rust/tools/gen_provider_table.py` from `packages/ai/src/providers`) gives 32 single-API providers their wire API, base URL, and key variable; `azure`, `amazon-bedrock` (`AWS_REGION` plus `AWS_BEARER_TOKEN_BEDROCK`, or `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`), and `google-vertex` (`GOOGLE_CLOUD_API_KEY`) have their own settings; `{NAME}` placeholders in base URLs (Cloudflare account id) come from the environment. The key comes from `--api-key`, then the environment, then the stored login; `nyan login <provider>` stores a typed key (not checked) for any of them. `openai` now uses Responses, as the TypeScript provider does.
- Not supported, and refused with the reason instead of guessed: providers whose models mix wire APIs (`openrouter`, `opencode`, `opencode-go`, `fireworks`, `github-copilot`, `cloudflare-ai-gateway`; `azure` has one chat-completions model of 43) because choosing the API needs the per-model catalog; `openai-codex` (needs OAuth and the codex-responses API); Anthropic OAuth tokens; Vertex Application Default Credentials; Bedrock named profiles, ECS and IRSA roles, and SSO; `typesafe` (no chat models). Not ported from the TypeScript adapters in any protocol: thinking/reasoning blocks and their replay signatures, images, prompt-cache controls (Anthropic `cache_control`, session ids), per-provider chat-completions compat quirks (`max_completion_tokens`, developer role, reasoning fields), retry-after handling, `max_tokens` (Anthropic sends a fixed 16,384), and tool-name casing for OAuth. The model catalog (context windows, per-model API, pricing) does not exist yet.
- Not implemented: credential and model resolution beyond one environment variable, a model catalog or `models.json` (the OpenAI base URL is fixed), OAuth, MCP, skills, prompt templates, settings, and trust.
- The Windows build of the HTTP crate (it compiles C code for the rustls crypto provider), the Windows shell lookup for `bash`, and the NASM install step in the workflow are all unverified until the Windows runner executes.

### RS6 to RS8

Not started.
