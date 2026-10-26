# Rust Coding Agent 交接：core-first parity preview（2026-10-26）

这是从公司环境继续工作的入口。代码已经提交到本地 `main`，但没有直接推送受保护的 `main`；本页提交后会把完整历史推送到远端 `rust-core-preview`。

## 到公司后的第一步

```bash
git fetch origin rust-core-preview
git switch -c rust-core-preview --track origin/rust-core-preview
# 或者已经有同名分支时：
git switch rust-core-preview
git pull --ff-only

sed -n '1,320p' ROADMAP.md
sed -n '1,360p' docs/handoff/2026-10-26-rust-core-preview.md
```

不要把这条线直接推到 `main`。`main` 受保护，必须通过 PR 的 Merge gate。

## 当前状态快照

| 项 | 状态 |
| --- | --- |
| 产品方向 | Rust-only、core-first；TUI/GPUI 最后做，Rust CLI 普通运行路径不依赖 Node |
| 当前本地分支 | `main`，相对 `origin/main` ahead 45；推送目标是 `origin/rust-core-preview` |
| 最新功能提交 | `9c532c706 feat: replay multimodal content in Responses requests` |
| 本交接提交 | 本文件所在提交（拉取远端后以 `git log` 为准） |
| Rust 测试 | workspace 358 个测试通过 |
| Linux 检查 | `cargo fmt`、workspace `cargo clippy --all-targets --locked -- -D warnings`、workspace `cargo test --workspace --locked` 通过 |
| 仓库检查 | `npm run check` 通过；每个提交的 pre-commit hook 也通过 |
| Windows | 尚未在真实 Windows runner 执行；本机交叉检查因缺少 `x86_64-w64-mingw32-gcc` 被 `aws-lc-sys` 阻塞 |
| 凭据/真实 provider | 没有使用真实 provider 凭据；provider 测试使用 loopback/fake server |
| 发布/推送 | 没有发布、没有 tag、没有 push 到 `main` |

## 已完成的路线图工作

### RS0–RS2 基础

已完成并已有历史提交：

- Rust workspace、core/runtime/provider/session/tools/protocol/CLI crate 边界；
- agent loop、sequential/parallel tools、retry backoff、取消和未知结果；
- POSIX 信号取消，Windows 代码已完成交叉编译检查但没有真实 runner 证据；
- OpenAI Completions/Responses、Anthropic Messages、Gemini Generative AI/Vertex、OpenAI Codex Responses；
- auth、OAuth/Codex 登录刷新、models.dev catalog/cache、内置工具、print/JSON 模式；
- 删除 owner 明确不需要的 Azure、Bedrock、SigV4、event-stream、Mistral 代码。

### RS4 crash safety / filesystem

已完成：

- session 使用 OS advisory lock；进程被杀死后内核释放锁；
- auth 使用 `auth.json.flock` OS lock；
- 新增 `rust/crates/nyan-coding-agent-fs` 原子替换 helper；
- `write`/`edit`/auth 使用原子写，保留 symlink/mode，并包含 Windows rename retry；
- `edit` 拒绝非法 UTF-8，避免有损写回；
- session crash-lock 测试会杀死持锁进程并验证后续可以重新打开。

### RS3 message/content/retry

已完成的可逆基础：

- `ContentBlock`：`Text`、`Thinking`、`Image`、`ToolCall`；
- `Message.content` 有序内容；旧 `text`/`tool_calls` 字段暂时保留为迁移兼容视图，不能宣称已经完成最终模型切换；
- runtime 流式文本、tool call 会同步写入有序 content 和兼容字段；
- session message codec v3，支持 content blocks；可读取旧 v2 flat message；
- Anthropic 历史请求 replay：thinking、redacted thinking、image、text、tool use；
- Gemini/Vertex 历史请求 replay：text、image、functionCall；thinking 仍按现有规则隐藏；
- OpenAI Chat Completions 历史 replay：user/system multimodal parts、assistant text/tool calls、tool output；
- OpenAI Responses 历史 replay：input text/image、assistant output text/function call、tool output；Codex Responses 共享该输入路径；
- `Retry-After` 支持 milliseconds、seconds 和 HTTP-date；server delay 覆盖本地指数退避，并受 runtime cap 限制；
- provider 错误分类：rate limit、overload、context overflow、media too large、tool pairing、connection，以及原有 auth/model/request/stream；
- runtime 稳定错误 code 已覆盖新分类；
- 新测试均做过 mutation check，删除关键逻辑会使对应测试失败。

### RS7 read guards

已完成：

- `read` 在打开前拒绝目录、设备、FIFO 等非 regular file，避免 FIFO 阻塞；
- 16 MiB 输入上限，防止任意大文件进入 agent 进程；
- UTF-16 little-endian/big-endian BOM 解码；
- 保留现有文本截断和 image refusal 行为。

### RS4 entry projection

已完成最小线性模型：

- `SessionEntryId` 由 journal sequence 稳定生成；
- `SessionEntry` 包含 `id`、`parent_id`、sequence、kind、原始 payload、可选 decoded message；
- `SessionJournal::entries()`；
- `NativeSession::get_entries()`、`get_tree()`、`get_fork_messages()`；
- 当前 journal 没有 branch records，因此 tree 是线性 parent chain；不能宣称已经完成 TypeScript tree/fork parity。

## 关键设计约束

1. **core 先于前端。** 不要现在开始 TUI/GPUI；先完成 model/session/behavior/resources/tools/MCP/session API。
2. **不要把 compatibility fields 当最终模型。** Provider request builders 正在逐个迁移到 `Message::model_content()`；后续应继续移除读取旧字段的路径，最终再考虑删除兼容字段。
3. **所有 provider 测试走 loopback/fake server。** 不启用真实 API key，不做真实 token 消费。
4. **新增测试必须有 mutation evidence。** 测试必须验证契约，不要只验证 mock call。
5. **Rust workspace 仍按 exact dependency、`default-features = false`、`unsafe_code = forbid` 执行。**
6. **不要直接修改全局 Agent 配置，不要从 Pi/Rebon/Comet 复制代码。**

## 下一步，按顺序继续

### 1. RS3 streaming content events

当前 provider stream 仍主要只有：

- `TextDelta`；
- complete `ToolCall`；
- `Usage`；
- `Finished`。

下一步应增加 neutral streaming events：

- thinking start/delta/end；
- toolcall start/delta/end；
- thinking signature/redacted payload；
- usage extensions（reasoning/cost 后续再处理）。

推荐先做 Anthropic streaming thinking：

- `content_block_start` 的 `thinking` / `redacted_thinking` 记录 block 状态；
- `content_block_delta` 支持 `thinking_delta`；
- runtime 用有序 `ContentBlock::Thinking` 累积；
- JSON/protocol 层之后再补 `thinking_*` 事件，避免一次把所有 surface 改完。

必须补：loopback fixtures、runtime state-machine tests、mutation checks，并确认 signed/redacted thinking replay 规则。

### 2. RS3 remaining model data

- usage cost/reasoning/cache retention fields；
- provider model thinking capabilities；
- Codex `reasoning`/service tier；
- prompt-cache control；
- error classification 的 provider-specific body matrix；
- stream watchdog 和 non-streaming fallback；
- `max_tokens` context overflow shrink retry（不计入普通 backoff）。

### 3. RS4 cwd sidecar and session API wiring

当前 cwd key 已 canonicalize、处理 symlink 和 Windows `\\?\\`/case 规则，但还没有 sidecar 记录真实 cwd。继续做：

- 在 cwd-hash session directory 写入原子 sidecar，记录真实 cwd；
- sidecar 读取失败要明确报告，不要静默切换到其他 cwd；
- 将 `get_entries`/`get_tree`/`get_fork_messages` 接入后续 front-end-neutral session API；
- 再做 `--resume` listing、`--fork`、export/import。

### 4. Track T0/T1

- T0 需要 owner/外部协作：从 `rust-core-preview` 开 PR，让 GitHub Windows runner 真实执行 `rust.yml`；不要直接 push `main`。
- 本地 Windows cross check 目前缺少 MinGW C compiler，不能把本机失败修成“代码失败”；应记录为环境限制。
- T1 differential harness：先选一个 wire API，录制 TypeScript/Rust request/event/session fixture，normalize timestamps/ids 后比较。

### 5. 后续阶段

- RS5：steer/follow-up queues、thinking levels、compaction、shell commands、hooks；
- RS6：settings/resources/trust/skills/prompt templates/proxy/CA；
- RS7：PowerShell、tool_search、MCP client、MCP OAuth/SSRF/reconnect/trust；
- RS8：front-end-neutral session API、render projection、23 events、33 RPC commands；
- RS9：extension boundary，等待 decision #56；
- RS10：Windows evidence、fuzzing、packaging/headless preview；
- RS11：最后再做 terminal TUI；GPUI 只能作为额外 desktop client spike，不能替代 terminal mode。

## Owner decisions：不要自行关闭

这些决定按 `ROADMAP.md` 保持开放，继续做推荐的可逆部分：

- **#54 Rust MSRV**：当前已按 `rust-version = 1.89` 实现 `File::try_lock`，等待 owner 最终确认；
- **#55 terminal UI / GPUI / both**：在 G-UI spike 决定；terminal parity 不能被 GPUI 替代；
- **#56 extension boundary**：推荐 typed JSONL out-of-process bridge；
- **#57 codemode**：暂不把 JS engine 放进 core；
- **#58 tasks/agents**：M1 out of scope；
- **#59 SQLite import**：推荐先做 legacy JSONL importer；
- **#60 provider long tail**：按需逐个添加；
- **#61 plain REPL**：可作为开发工具，默认关闭；
- **#62 release signing/update channel**：当前先 SHA-256 + provenance，签名后置。

## Rebon-Code / CometixCode 研究边界

### Rebon-Code

可以继续学习：

- projection 与 paint 分离；
- streaming Markdown stable-prefix；
- selector/keybinding/paste state machine；
- shell policy、truncation、atomic write、Retry-After、MCP 安全边界；
- session tail metadata 和 compaction recovery。

不要直接搬：engine/kernel/seat、Boa/Verve、Windows sandbox、SSE parser，以及默认 hosted endpoints/第三方 OAuth client id。只有明确 Apache-2.0 且确实是小型原创模块时，才可保留版权/NOTICE 并标记改动。

### CometixCode

按 clean-room 学习行为、边界、fixture、golden transcript、edge-case checklist 和偏差登记；不逐文件翻译源码，不把其 Claude Code-specific prompt/tool description 当作 Nyan 契约。

## 重要检查命令

```bash
cargo fmt --manifest-path rust/Cargo.toml --all
cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path rust/Cargo.toml --workspace --locked
npm run check

git status --short --branch
git log --oneline -12
```

不要运行 `npm run build` 或全量 `npm test`，除非 owner 明确要求。不要使用 `git reset --hard`、`git clean`、`git stash`、`git add -A`、`git add .` 或 force-push。

## 当前提交线

从较近到较早：

```text
9c532c706 feat: replay multimodal content in Responses requests
11566f712 feat: replay multimodal content in OpenAI chat requests
a61dd4a48 feat: replay ordered content blocks in Gemini requests
6150e741e feat: replay ordered content blocks in Anthropic requests
60862de90 feat: classify provider failures for stable retry outcomes
ca30931ef feat: add linear session entry projections
c87d59839 feat: extend core content, retry, session, and read safety
6cb9159b2 feat: crash-safe session and credential locks, atomic file replacement, edit refuses non-UTF-8 files
```

本交接文档的提交会在这些提交之后。拉取远端后以 `git log` 为准。
