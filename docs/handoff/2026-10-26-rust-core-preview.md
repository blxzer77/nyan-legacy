# Rust Coding Agent 交接：core-first parity preview（2026-10-26）

这是当前 `rust-core-preview` 的交接入口。后续工作继续保持 Rust-only、core-first；不要把这条线直接推到受保护的 `main`。

## 接手后的第一步

```bash
git fetch origin rust-core-preview
git switch rust-core-preview
git pull --ff-only
sed -n '1,360p' docs/handoff/2026-10-26-rust-core-preview.md
sed -n '1,260p' ROADMAP.md
git status --short --branch
```

目标分支是 `origin/rust-core-preview`。`main` 受保护，必须通过 PR 的 Merge gate。

## 当前状态快照

| 项 | 状态 |
| --- | --- |
| 当前分支 | `rust-core-preview` |
| 最新提交 | 见 `git log --oneline -12`；本交接提交位于 `99eda5bc9` 之后 |
| 已推送提交 | 本交接完成后，当前分支的全部已跟踪修改推送到 `origin/rust-core-preview` |
| Rust 方向 | Rust-only、core-first；TUI/GPUI 最后做，普通 Rust 运行路径不依赖 Node |
| 验证 | `cargo fmt`、workspace clippy、workspace tests、`npm run check` 全部通过 |
| Windows | 尚未获得真实 Windows runner 证据；按当前约定暂不阻塞本阶段 |
| provider/凭据 | 只使用 loopback/fake provider；没有使用真实 provider 凭据 |
| 未跟踪目录 | `eval/` 是外部未跟踪目录，不能修改、暂不加入提交，也不应删除 |
| 发布 | 没有 tag、没有 npm publish；只推送 preview 分支 |

## 已完成工作

### RS0–RS2 基础

- Rust workspace 和 crate 边界：core、runtime、provider、provider-http、session、tools、protocol、CLI 等；
- agent loop、顺序/并行工具执行、retry backoff、取消、未知结果和进程组生命周期；
- OpenAI Completions/Responses、Anthropic Messages、Gemini Generative AI/Vertex、OpenAI Codex Responses；
- auth、OAuth/Codex 登录刷新、models.dev catalog/cache、内置工具、print/JSON 模式、信号退出；
- crash-safe session/auth locks、原子文件替换、symlink/mode 保留、Windows rename retry；
- `read` 的 regular-file/device/FIFO/size/UTF-16 BOM 防护，`edit` 拒绝非法 UTF-8。

### RS3 model/request parity

- 有序 `ContentBlock`：text、thinking、redacted thinking、image、tool call；
- thinking 流式更新、signature/redacted block、ordered content replay；
- OpenAI、Anthropic、Gemini/Vertex 的 reasoning usage；
- reasoning effort、prompt-cache controls、service tier、output limits；
- context overflow 时只缩小显式配置的 output limit；
- catalog-priced usage cost，以 fixed-point micro-dollar 持久化；
- OpenAI Chat/Responses/Codex、Anthropic、Gemini/Vertex 请求体和非流式 JSON fallback；
- provider retry/error classification、loopback tests。

### RS4 sessions

- canonical cwd sidecar：原子写入、`0600` 权限、稳定 `session_cwd_sidecar` 错误；
- `SessionCommand`：`GetEntries`、`GetTree`、`GetForkMessages`、`GetSessionName`、`GetSessionStats`、`SetSessionName`；
- session entries/tree projection、stats、entry-based fork、clone、CLI `--fork`；
- `SessionSummary`、lock-free session listing、resume data；
- Rust JSONL export/import、legacy TypeScript JSONL importer、provider-free `--export`；
- journal replay、locks、incomplete-run recovery、corruption handling。

### RS5 agent behavior

- `AgentControl::steer`、`follow_up`、`snapshot`、`clear_queue`、`abort`；
- `QueueMode::{OneAtATime, All}` 和 `queue_update` JSONL event；
- follow-up turn 与 safe turn-boundary steering injection；
- thinking levels：`off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`；
- query/set/cycle API，以及 `thinking_level_changed` event；
- `RuntimeHooks::before_agent_start`、`before_tool_call`、`after_tool_call`；
- before-hook fail-closed，blocked tool 变成 model-visible error，不执行外部 effect；
- 手动 summary compaction：保留 system prompt、插入 summary、保留 suffix，发出 compaction events；
- 自动 compaction：`CompactionPolicy`、context token estimate、`ContextCompactor` owner；
- context overflow 时优先尝试 compaction，再尝试 output-limit shrink retry。

## 最近提交

```text
99eda5bc9 feat(runtime): add automatic compaction owner
5192a5c26 feat(runtime): add summary-based compaction
b034a458b feat(runtime): add lifecycle hooks
7f6b98952 feat(runtime): add thinking level control
f3a447781 test(runtime): cover queued follow-up turns
a51245cf5 feat(runtime): add steering and follow-up queues
c23fb726e feat(session): add export import and resume data
85901cab6 docs: update Rust session option status
875927b8e feat(session): enable entry-based forks
9eb323e93 feat(session): add neutral session commands
9ed01dfba feat(session): add cwd sidecars
4c61020f1 feat(provider): support non-streaming responses
```

## Compaction 当前设计

`rust/crates/nyan-coding-agent-runtime/src/lib.rs` 提供：

```rust
pub struct CompactionPolicy {
    pub context_window: u64,
    pub reserve_tokens: u64,
    pub keep_recent_messages: usize,
}

pub trait ContextCompactor: Send + Sync {
    fn summarize(
        &self,
        messages: Vec<Message>,
        cancellation: CancellationToken,
    ) -> BoxFuture<Result<String, String>>;
}
```

runtime 不直接决定 provider summarization 请求格式；宿主通过 `with_compactor` 注入 summary owner。这样可以继续明确 provider admission、重试、取消和持久化责任，避免在 agent loop 中硬编码 provider 行为。

当前还没有完成：

- 真实 summarization provider request 和 bounded retry；
- summarization retry events；
- compaction journal/session persistence 和 crash recovery；
- 大 tool result spill-to-file；
- automatic compaction 的 TypeScript differential fixture。

## 接下来优先级

### 1. 完成 RS5 compaction 生命周期

- 将 compaction summary 写入 session journal，而不是只修改 runtime memory；
- session reopen 后保留 summary 和 retained suffix，且 metadata/recovery 可重建；
- 为 summarization provider 调用加入 cancellation、bounded retry、context-overflow retry；
- 明确 summary request 的失败和 unknown outcome，禁止误报成功；
- 增加 overflow → compaction → retry、compaction failure、cancel during summary 的 mutation-oriented tests。

### 2. 实现 shell command parity

- `!`：执行 bash 并把结果作为用户侧事件/消息处理；
- `!!`：执行 bash 并把结果注入后续 agent turn；
- `bash_execution_update` JSONL events；
- `abort_bash` control path；
- 取消、超时、进程组终止和 unknown-outcome 语义；
- 不执行真实 provider，使用 fake/loopback provider 验证完整事件序列。

### 3. 完成 provider thinking capability mapping

- catalog/provider/model 的支持 level 映射；
- provider-specific normalization；
- unsupported thinking request 的稳定错误；
- model switch 和 level cycle 的行为测试。

### 4. 开始 RS6（与 RS5 非重叠部分可并行）

建议新增 `rust/crates/nyan-coding-agent-config` 和 `rust/crates/nyan-coding-agent-resources`，不要把配置逻辑放进 agent loop：

- global/project `settings.json` merge；
- per-key invalid tolerance；
- unknown fields preservation；
- atomic write 和 cross-process lock；
- `models.json`、trust store、`SYSTEM.md`、`APPEND_SYSTEM.md`；
- skills/prompt-template discovery、frontmatter、de-duplication；
- themes/keybindings parse/validate；
- proxy、NO_PROXY、SOCKS、extra CA；
- loopback proxy/CA 和 untrusted-project fixtures。

RS6 写集按 `ROADMAP.md`：新 config/resources crates 和 `-auth`；不要修改 runtime/session-api surfaces，除非先协调写集。

### 5. RS8/RPC 后续

将 queue control、compaction、session listing/stats、export/fork 等接入统一 front-end-neutral session API 和 RPC。当前 JSONL/runtime primitives 已存在，但完整 RPC/interactive surface 还未完成。

## 约束

1. 只在 `rust-core-preview` 工作；不要修改 protected `main`。
2. 不修改、不 staging、不删除 `eval/`。
3. provider 测试使用 loopback/fake server；不要读取或写入真实凭据。
4. 新测试必须是 mutation-oriented，验证行为契约而非 mock 调用。
5. 修复 clippy/lint，不使用 suppression 掩盖问题。
6. 依赖继续使用 exact version、`default-features = false`、crate `unsafe_code = forbid`。
7. 不运行 `npm run build` 或全量 `npm test`，除非 owner 明确要求。
8. 不使用 `git reset --hard`、`git checkout .`、`git clean -fd`、`git stash`、`git add -A`、`git add .` 或 force-push。
9. 不自行关闭 `ROADMAP.md` 中的 owner decisions #54–#62。

## 验证命令

```bash
. "$HOME/.cargo/env"
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path rust/Cargo.toml --workspace --locked
npm run check
git diff --check
git status --short --branch
```

本次交接前已运行以上完整 Rust/TypeScript gates，结果全部通过。Windows runner evidence 仍未验证。
