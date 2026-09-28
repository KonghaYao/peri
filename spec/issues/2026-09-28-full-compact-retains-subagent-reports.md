# Full Compact 遗漏系统通知中的报告，压缩后仍保留大段旧上下文

**状态**：代码修复及回归验证完成；待现场 provider usage 验收。用户已授权提交。
**创建日期**：2026-09-28（Asia/Shanghai）。
**范围**：Full Compact 的摘要输入、成功后的活跃上下文替换及持久化恢复。

## 用户反馈与已确认目标

用户反馈：会话已有约 3000 条 messages，compact 后上下文占用仍有约 50%。随后明确：

> full compact 是 fork 上下文输出结果，所以肯定是我们处理错误了；然后compact 之后，这些报告也肯定不要的呀，记录到 issue 里面

本 issue 按此目标推进：Full Compact 从当前模型上下文的快照派生摘要请求，由摘要承接历史工作；子 Agent 报告等历史结果必须参与摘要，成功提交后原报告退出后续模型的活跃上下文。不能因为报告存为 `system_reminder` 就跳过摘要、永久保留全文。

这里的 fork 是压缩计算使用上下文快照的语义，不预先规定使用 ACP `session/fork` 或创建持久化子会话。原历史可保留在存储中供回查；“compact 后不要报告”指不再把旧报告全文重复发送给模型。

## 修复前已确认的实现缺陷

事实源：

- [full.rs](../../peri-agent/src/agent/compact_v2/full.rs)：`full_compact_inner`、原 `preprocess_messages_for_summary`（已删除）。
- [transcript.rs](../../peri-agent/src/session/transcript.rs)：`TranscriptEntry::as_message`、`visible_messages`、`visible_model_messages`。
- [full_test.rs](../../peri-agent/src/agent/compact_v2/full_test.rs)：`test_full_compact_summarizes_and_excludes_canonical_report`。

修复前数据流：

1. 子 Agent 完成通知等内容以 canonical `TranscriptEntry::Reminder` 持久化，正文可能包含整份报告。
2. Full 读取 `visible_messages()`，它通过 `as_message()` 跳过所有 canonical reminder。因此这些报告不进入摘要输入。
3. Full 的 excluded 集合同样只接受 `as_message()` 返回的非 System 普通消息，历史报告不会被排除。
4. Reason 的模型视图仍投影可见 reminder，所以 Full 成功后报告继续全文进入后续请求。
5. 再次 Full 仍重复跳过相同报告；新报告持续追加，形成随会话增长、无法被 Full 回收的上下文基线。

此外，修复前摘要调用是把普通消息逐条预处理后拼成文本，构造独立的两条消息请求；每条正文按 2000 字符预处理。该实现不能作为“完整 fork 当前模型上下文”的证据。修复必须同时核对输入覆盖与内容完整性，不能只把 reminder 加进 excluded 集合，也不能先截掉报告关键结论再声称已摘要。

原 `full_compact_preserves_canonical_reminder_without_flags` 测试锁定了旧行为；现已改为 `test_full_compact_summarizes_and_excludes_canonical_report`，验证报告尾部进入摘要且成功后排除。

## 本地只读观测

2026-09-28 以 SQLite `mode=ro` 查询 `~/.peri/threads/threads.db`。样本为当前 Peri 工作区会话 `01a0e1d8-7973-7c00-82dc-4b4e747f3644`。

| 项目 | 数量 |
| --- | ---: |
| 持久化记录总数 | 3640 |
| 已 excluded 的普通消息 | 3542 |
| 未 excluded 的普通消息 | 7 |
| 未 excluded 的 canonical reminder | 91 |
| 其中 `subagent/completed` | 23 |
| 这 23 条完成通知的正文字符合计 | 252664 |

91 条 reminder 全部包含 model audience；其中 90 条同时面向 TUI/diagnostics。正文长度来自解码持久化 JSON 后对 `reminder.body` 计字符，不含 JSON 外壳，不是 token 统计。查询未输出报告正文。

证据边界：此样本尚未被用户确认为反馈中的那个 session；未取得对应压缩前后真实 provider usage，不能据此声称已精确解释现场 50%。但“普通历史已大量排除、报告仍完整保留”的数据与上述代码路径一致。此前演示的 200k 窗口和 85k 报告等 token 数仅为说明性假设，不是观测结果。

## 修复契约

1. **摘要输入覆盖模型上下文。** 以本次 Full 的确定快照为边界，覆盖其中模型可见的历史报告及通知语义，保留身份、顺序与可信来源。不能用普通消息列表替代完整模型上下文，也不能通过无声截断遗漏报告关键内容。
2. **成功后由摘要承接旧报告。** 快照内已纳入摘要的报告全文退出活跃模型视图，不因 canonical reminder 存储类型而豁免；后续 Full、下一轮 prompt 及冷恢复不得重新带回全文。
3. **先摘要，再原子切换。** 沿用 durable compaction lifecycle，将摘要追加与对应排除变更在同一事务提交。摘要失败、提交前取消不得丢失原报告；已确认提交后取消仍保留结果，提交不确定仍要求 reload。
4. **只处理本次快照。** Full 期间或之后新到达的结果不属于已摘要历史，不能被一起排除。仍有效约束应由摘要或相应权威状态承接，不能借本修复丢掉用户约束，也不能以此为由保留所有旧报告全文。
5. **保持所有权边界。** 当前会话自己的报告可随 Full 替换；不得改写父会话的 frozen/inherited 数据。涉及继承视图时须明确派生请求与本会话投影的处理方式，不能通过修改 ancestor flags 绕过现有边界。

本 issue 的主因是摘要输入与压缩后替换集合不完整。调低触发阈值、降低摘要上限、清空 token 计数或只修 TUI 百分比，均不构成本问题的修复。摘要、文件、Skill 回注的统一预算可另行评估，不作为修复报告残留的前置条件。

## 验收与回归

- [x] 构造含普通消息和大段 `subagent/completed` 报告的 transcript，在捕获到的实际摘要请求中断言报告关键事实可见，包括超过当前 2000 字符截断位置的尾部结论。
- [x] 摘要成功提交后，下一次真实 Reason 请求包含摘要且不再包含旧报告全文；断言 provider-facing 请求，不只断言 `visible_messages()` 数量下降。
- [x] 只有报告、没有普通对话的模型上下文仍可正确摘要，不能进入 “No conversation history to compact” 分支。
- [x] 连续两次 Full、自动 compact 与手动 `/compact` 均满足相同替换语义，不累计或复活旧报告。
- [x] SQLite 冷加载恢复 excluded 状态与摘要，原始报告仍可作为历史回查；活跃模型视图中无全文残留。
- [x] 覆盖摘要失败、提交前取消、提交后取消和提交结果不确定；不得先排除再丢摘要，也不得在已提交后伪回滚。
- [x] 验证 Full 快照之后新到达的报告仍保留，摘要切换不丢失新工作；继承场景不修改父会话数据。
- [x] 3000 条历史、23 份长报告经自动 Full 后，捕获实际 `ModelRequest`：摘要请求含每份报告尾部；下一次 Reason 请求包含摘要和 System 指令，不含报告全文；canonical 存储保留旧报告及 excluded flags。
- [ ] 现场 provider input usage 验收：尚未调用线上模型或测量实际 token 降幅；固定模型替身只证明请求内容与生命周期，不证明摘要质量或“50% 降至多少”。

## 实施计划与审查重点

1. 先建立报告尾部内容未进入摘要、成功后报告未排除的失败回归。
2. Full 从可见模型消息快照构建结构化摘要请求，复用正常模型消息转换与已提交 Micro 投影恢复，删除旧的逐条文本预览截断。摘要完整有效后，原子提交 own region 中普通历史与 reminder 的 excluded transitions；System 和 ancestor 的所有权不变。
3. 手动 `/compact` 从一致存储快照恢复完整 payload、flags 与 ancestor/own 边界。普通消息 ID 校验仍保留，但不能把该校验投影当成实际摘要输入；仅有 reminder 的历史也必须可压缩。
4. 审查失败/取消/冷恢复、仅 reminder、重复 Full、新到达结果、继承边界，以及正常 Reason 的实际请求。用确定的模型替身验证请求内容，真实 SQLite 验证持久化；不对线上会话执行试验性压缩。
5. 同步现行契约与入口文档，运行相关 Agent/ACP 回归、格式、lint 和 doc tests，审查 staged diff 后仅提交本任务文件。未取得线上 provider usage 时保留现场验收项。

## 代码审查与验证结果

审查范围：Full 输入到原子提交、手动 pipeline 到 host 恢复、共享 Reason 投影、摘要输出有效性、ancestor/own 边界及测试断言。修正了手动 pipeline 丢 reminder 的第二处缺陷，删除独立有损预览；摘要只含 analysis 或以非 EndTurn 停止时拒绝提交，防止用空白/截断摘要替代报告。已有 Micro 投影由 Full/Reason 共用实现恢复。测试补充 Full 调用期间 inbox 新结果的边界，以及真实 SQLite 提交前/后取消和提交确认丢失。

| 验证 | 结果 |
| --- | --- |
| 修复前目标回归 `test_full_compact_summarizes_and_excludes_canonical_report` | 1 项按预期失败：摘要请求缺少报告尾部；exit 101 |
| `cargo test -p peri-agent --lib` | 861 passed，exit 0 |
| `cargo test -p peri-acp --lib compact` | 53 passed，exit 0 |
| `cargo test -p peri-acp --test compact_command_contract_test` | 2 passed，exit 0 |
| `cargo test -p peri-agent -p peri-acp --doc` | Agent 10 passed；ACP 无 doc tests；exit 0 |
| `cargo clippy -p peri-agent -p peri-acp --all-targets -- -D warnings` | passed，exit 0；审查中将测试锁 guard 改为词法作用域后通过 |
| `lefthook run pre-commit` | check / clippy / fmt / layer-imports / typos 全部通过，exit 0 |
| `bash scripts/check-file-size.sh` | exit 1：43 个既有超限文件（源码 5 / 测试 38）；本次修改/新增的 Rust 文件全部 ≤1000 行 |

提交 hook 首次复跑遇到共享 target 中 `libc` / `thiserror_impl` 构建产物消失，同时观察到 `cargo clean` 进程；该次提交未生成 commit。提交检查改用独立 `CARGO_TARGET_DIR`，不清理或中断其他工作。此前成功的测试与检查结果按其实际退出状态记录。

测试使用临时 SQLite/工作区与模型边界替身，未写入用户的本地会话库。手动冷恢复测试释放旧门面和 lease 后重新打开数据库；没有声称完成真实线上模型或跨进程端到端验收。新摘要请求携带完整可见历史，对辅助模型的上下文窗口/模态支持仍需现场确认；provider 拒绝请求时保留原历史，不回退到无声截断报告。

## PR 审查补充

[PR #174](https://github.com/KonghaYao/peri/pull/174) 的审查发现：子会话只有继承报告、没有可替换 own 历史时，恢复后的模型视图仍会触发摘要调用。已补充 `test_full_report_inherited_only_skips_summary_model`，修复前按预期因模型被调用而失败（exit 101）；Full 增加 own 历史条件后沿用空历史 fallback，保留祖先标记和原有模型可见性检查。Agent 全量单元测试 862 项、ACP compact 测试 53 项及相关 crate 的 all-targets Clippy 均通过（exit 0）。

## 关联与文档路由

- [Full/Micro churn issue](2026-09-10-p0-full-micro-compact-churn.md) 的 A5 已识别 reminder 基线风险；本 issue 补充本地规模证据及用户确认的 Full 语义，独立承接报告残留修复。
- 已同步 [ARC-COMPACT-001](../../docs/standards/architecture-contracts.md)、[Micro Compact 设计](../../docs/design/micro-compact.md) 中相关说明、[Agent code-index](../../docs/code-index/peri-agent.md) 与旧 reminder 保留测试。既有“不能靠删 reminder 伪造预算进展”仍不等于“成功摘要后必须永久保留报告”；须明确区分。
- 初次记录仅进行了代码阅读与本地数据库只读观测；后续实施验证结果在本 issue 更新。
