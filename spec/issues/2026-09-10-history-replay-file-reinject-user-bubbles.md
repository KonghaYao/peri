# 历史恢复将“最近读取的文件”内部上下文显示为用户发言

**状态**：Partial
**优先级**：中
**创建日期**：2026-09-10

## 问题描述

用户报告历史恢复时持续出现 `[最近读取的文件: ...]` 和大段源码，要求追查到底。截图路径为 `peri-agent/src/session/subagent_test.rs`，正文以 subagent 统一入口测试的模块注释开头。内部文件上下文不应冒充用户发言。

## 症状详情与实机证据

只读查询本机 `~/.peri/threads/threads.db`，未修改会话数据。

- 会话 `01a08abd-adab-7891-a3f9-3d80e92367ea` 中找到与截图路径、首段文字一致的三条记录，均为 `role=user`，每条正文 20095 字符。
- 消息 ID：`01a08bc2-98d9-7a81-b722-69fed0534b3d`、`01a08bc3-faf3-75c0-9818-597afda1368f`、`01a08bc5-9990-7362-be34-5c159e1d6861`；对应 excluded 为 1、1、0。
- 该会话共有 20 条以该文件回注前缀开头的记录：10 条 excluded，10 条仍有效。这里统计的是全部文件，不只是截图中的一个文件。
- 本次数据库快照中，共 2239 条消息正文以该前缀开头，分布于 281 个 thread。这是存储规模，不代表全部 thread 都经用户恢复过。

## 根因与完整链路

1. `peri-agent/src/agent/compact_v2/full.rs::collect_reinject_v2` 从可见 Read 历史提取路径，读取文件并构造 `[最近读取的文件: 路径]\n正文`，调用 `BaseMessage::human`。相同逻辑还用于 `[激活的 Skill 指令: ...]`。
2. 代码注释说明选择 Human 是为了避免 System 消息被模型调用层提升到 frozen prompt。模型角色选择同时被用作用户来源身份，内部来源信息没有进入载荷。
3. `full_compact_inner` 通过 `commit_compaction_lifecycle` 把摘要、回注消息和旧消息排除标记一起提交。回注因此成为持久化历史，而非只存在于当前模型请求的临时内容。
4. `peri-acp/src/host/requests/session_lifecycle.rs::handle_load` 经 `dispatch::load_session_payloads` 加载 payload 并调用 `replay_persisted_session_history`。
5. `peri-resources/src/sessions/sqlite_store.rs::load_context_payloads` 经 `load_payloads` 读取完整消息；查询没有 excluded 条件，返回的 payload 也不携带这些标记。
6. `peri-acp/src/dispatch/session_replay.rs::replay_session_history` 仅跳过 System；所有 Human 均映射为 `SessionUpdate::UserMessageChunk`，没有内部上下文分支。
7. `peri-tui/src/kit/acp_notifier.rs` 将 `user_message_chunk` 转成 `LocalUserBubble`，与普通用户输入共用显示路径。

结论：内部上下文与真实用户消息共用了 Human 持久化类型，历史回放直接按模型角色决定显示身份。完整历史加载让多次 Compact 留下的回注都重新可见，放大症状。即使过滤 excluded，仍有效的回注也会泄露到用户气泡，因此 excluded 不是根治点。恢复加载本身不执行上述文件重读，显示的是先前 Compact 已存下的正文。

历史定位：`git blame` 显示 `BaseMessage::human(human_content)` 及避免 frozen prompt 污染的注释来自 `15269e31c6`（2026-06-27）；这证明相关实现早已存在，不将此次报告简单归因于最近的 compact churn 修复。未进行版本二分，首次用户可见故障版本未确认。

## 最小复现与验证

在 `peri-acp/src/dispatch/session_replay_test.rs` 临时追加下列测试，复用实际回放函数和现有 CollectSender；调查结束已移除探针，保留代码于本文以便复现。

```rust
#[tokio::test]
async fn test_investigation_compact_file_reinject_leaks_as_user() {
    let updates = collect_replay(vec![BaseMessage::human(
        "[最近读取的文件: /src/main.rs]\nfn main() {}",
    )]).await;
    assert!(updates.is_empty(), "内部文件回注被回放成用户消息: {updates:?}");
}
```

命令：`cargo test -p peri-acp --lib -- test_investigation_compact_file_reinject_leaks_as_user`。

两次终态均为 exit 101，实际各执行 1 个用例，0 passed / 1 failed，607 filtered out。失败输出明确包含 `UserMessageChunk`，文本为上述文件回注正文，meta 为 `periReplay=true`。第二次测试执行耗时 0.00s，编译检查 0.29s。此失败是目标症状的复现证据，不是修复验证通过。

该探针锁定 ACP 映射边界；实机存储记录与源码链路补足 producer / persistence / TUI 证据。未运行真实 TUI 冷启动自动化，未修改生产逻辑，未宣称问题已修复。

## 修复方向与验收要求

- 为 Compact 回注保存结构化内部来源，使模型侧仍能收到所需上下文，而 replay 不再伪装用户发言。可评估现有 SystemReminder typed payload 是否适配，不能仅把 Human 改回 System。
- 历史数据没有来源字段，需要明确兼容策略；只按正文前缀全局过滤会误吞用户真实粘贴的同形内容，不应默认为可靠修复。
- 不应把所有 excluded 消息从 UI 历史删除：模型压缩可见性和用户浏览完整对话是不同语义。
- 验收覆盖：Full Compact → 落库 → 新 session/load；活跃及 excluded 回注；文件及 Skill 回注；真实同前缀用户输入；模型上下文保持；旧数据库兼容。
- 与 `2026-09-10-p0-full-micro-compact-churn.md` 相关但独立：减少重复 Compact 不会修复 Human 到用户气泡的错误映射。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-09-10 | — | Open | agent | 完成只读实机核对、链路追踪及两次最小复现；待实施修复 |
| 2026-09-10 | Open | Partial | agent | 传递与回放兼容已修复；首轮预算待处理 |

## 修复记录

### 修复 #1（2026-09-10）

- **用户范围**：先修传递时 System Reminder 丢失造成的 TUI / stdio 错误，沿用数据库格式；首轮上下文预算另行处理。
- **实现**：新增契约层 `compact_reminder::legacy_compact_reminders`，只匹配完整 plain-text Human 的文件/Skill 首行和 Compact 摘要续接标记；来源保留 Legacy，不提升为 trusted。模型通过 `TranscriptEntry::project_message` 编码 legacy envelope；ACP replay 改发 reminder，复用 MPSC/stdio 的 capability 分支。
- **正文保真**：8 KiB UTF-8 边界分块，统一 codec 转义 XML，保证文件正文不被标签提前闭合、不因 reminder 单条大小限制丢失。数据库正文、role 与 ID 均不改写；不做 schema 迁移。
- **兼容限制**：用户自行粘贴完全同形的内部格式无法可靠区分，按 Legacy 上下文展示；正文中的普通提及、多模态消息、缺损前缀保持原样。
- **验证状态**：目标回归、MPSC/stdio wire、相关测试与 workspace build/clippy 通过，详见下方。未 commit。
- **剩余工作**：恢复首轮预算预检未实施；因此整体 issue 为 Partial，而非全部修复。

### 修复验证

以下均核对终态退出状态；故障复现的旧探针和并行 compact 审计改动不属于本修复新增内容。

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-agent --lib -- test_legacy_compact_model_projection_preserves_storage` | 修复前 exit 101，1 failed；修复后 exit 0，1 passed |
| `cargo test -p peri-acp-types --lib -- compact_reminder` | exit 0，3 passed |
| `cargo test -p peri-acp --lib -- session_replay` | exit 0，5 passed，包括既有故障探针 |
| `cargo test -p peri-acp --lib -- compact_reminder_replay` | exit 0，2 passed；实际 MPSC 与 stdio JSON wire，均覆盖 capability true/false |
| `cargo test -p peri-agent --lib -- compact_v2 --quiet` | exit 0，163 passed，2 个既有审计用例 ignored |
| `cargo test -p peri-agent --lib -- session::transcript` | exit 0，45 passed |
| `cargo test -p peri-acp-types --lib -- system_reminder` | exit 0，29 passed |
| `cargo test -p peri-acp-types --doc` | exit 0，0 passed / 2 ignored；没有实际可执行 doc 用例 |
| `cargo test -p peri-agent --doc` | exit 0，1 passed |
| `cargo test -p peri-tui --lib -- reminder --test-threads=1 --quiet` | exit 0，33 passed；先前并发运行 32 passed / 1 failed（fold override），串行重跑通过，未修改其测试 |
| `cargo build --workspace` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |


## 追加调查：System Reminder 与恢复首轮预算

用户追问内部文件是否应由 System Reminder 承载，以及恢复首轮是否可能打爆上下文。

- `docs/design/system-reminder.md` 要求结构化来源、边界编码、不修改 frozen prompt、不冒充用户输入。当前文件/Skill 回注仍直接生成普通 Human，绕过 canonical reminder；不能只补 XML 文本标签代替 typed payload。
- UI replay 与模型投影不同。`peri-agent/src/session/exec/executor_helpers/v2_execute.rs` Phase 5 载入历史后，Phase 5.5 调用 `load_message_flags` / `set_flags_batch` 恢复标记；Reason 经 `visible_model_messages` 排除 excluded，再恢复 Micro projection。因此不能从 UI 显示 20 条推断模型实际收到 20 条。
- 另有首轮预算缺口：StageContext 的 TokenTracker 初始为 default，历史加载路径未用恢复消息估算占用；`estimated_context_tokens` 在没有 last_usage 时返回 None，Compact 使用 `unwrap_or(0)`，按零压力跳过自动压缩。Reason 的 Micro 投影不等于完整请求的硬性 token 上限检查。若有效历史本身已超限、切换更小窗口模型，或 flags 加载失败，存在首轮超窗风险。
- flags 读取错误当前只记 debug 后继续，不阻止模型请求。该错误路径可能使原本 excluded 的旧内容重新参与模型投影。这里只确认代码风险，未声称截图会话已发生该错误或真实超窗。
- System Reminder 解决来源与投递身份，不压缩 body；模型 audience 收到的文件正文仍消耗上下文。
- 验证：`cargo test -p peri-agent --lib -- test_estimated_context_tokens_none`，exit 0，实际 1 passed，728 filtered out。证明初始 tracker 为未知占用；首轮整体行为结论来自上述装配/Compact/Reason 静态链路，未执行超限 provider 请求。

后续修复须同时覆盖 canonical 回注、历史兼容，以及首轮模型请求的有效上下文预算预检，不能只修显示。

### 展示调整（2026-09-10）

用户要求保持简洁折叠风格，只区分类型。兼容 DTO 的 kind 分为 compact_file、compact_skill、compact_summary；所有分块保持原类型。TUI 对应显示“系统提醒 · 文件上下文 / Skill 指令 / 压缩摘要”，保留原颜色与折叠行为，不追加文件名或分段编号。Legacy provenance 不变。
