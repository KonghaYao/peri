# P1：Micro Compact 投影与工具分派的历史正文副本驻留

**状态**：已修复，待 PR 合并。
**本阶段**：两阶段均已实现（第一段借用式投影 + 单份共享快照；第二段独立于 token 的驻留字节预算）。
**优先级**：P1。
**类型**：性能/内存。
**创建日期**：2026-10-24。
**来源**：代码审计。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `e598d178`）。

## 确认证据

以下行号均以基线 `e598d17872bce487355e1bfa70f077743984760f` 为准。

1. **投影先整份复制，再截断/省略**：`peri-agent/src/agent/compact_v2/projection_archive.rs`
   - `:92` `let visible = transcript.visible_model_messages()?;`：`peri-agent/src/session/transcript.rs:315-321` 对每个可见条目调用 `TranscriptEntry::project_message()`，普通消息返回 `message.clone()`（`session/transcript/entry.rs:68-73`）。
   - `:98` 起遍历 owned 消息并应用投影；`:150-159` 的 `PreviewArchiveRead`、`:165-177` 的 `ResultReceipt`、`:201-203` 的 `OmitToolExchange` 都发生在正文已经复制之后。被归档/省略的工具结果先整份复制，再丢弃大部分或整条。
   - `:228` `pairs(&projected)` 与 `:464-513` 的 `pairs()` 通过 `content_blocks()`（`peri-acp-types/src/messages/content.rs:378-396`，`Text`/`Blocks`/`Raw` 均构造 owned 副本）扫描全部块。
   - `:19-31` 的 `safe_action()` 同样用 owned `content_blocks()` 做索引检查。
   - `projection.rs:587` 的 `project_content()` 再复制一次完整内容；`projection.rs:722-724` 对嵌套 `ToolResult` 先 `content.clone()` 再截断；`projection.rs:906` 的签名检查再次物化块列表。
2. **工具分派两次物化同一份历史**：`peri-agent/src/agent/stages/`
   - `invocation_context.rs:250` `messages: Arc::new(stage.visible_messages())`：副本 A（`StageContext::visible_messages()` 在 `stages/context.rs:241-249` 逐条 clone）。
   - `tool_dispatch.rs:212` 创建 `InvocationContext`；`tool_dispatch/execution.rs:389-393` 再执行 `ctx.messages().to_vec()` + `ai_msg.clone()`：副本 B。
   - A 与 B 跨整个工具批次执行持有；同一批内的多个工具共享 B，但 A 仍然常驻。
3. **峰值是线性多份，不是平方驻留**：单条可见消息在投影期可同时存在 canonical transcript、`visible_model_messages()` 副本与 `projected` 输出三份；工具分派期 canonical、`InvocationContext` 前缀快照与 dispatch 尾部快照至少三份。跨轮重复渲染使**累计复制成本**随历史长度与轮次呈平方增长；**峰值驻留**仍随副本数线性增长。
4. **24 MB 只是 planner 夹具**：`peri-agent/src/agent/compact_v2/planner_test.rs:630-634` 在 10,000 个交换、每条约 2,400 字节的夹具上打印 `canonical_body_bytes=24_000_000`。这是单元夹具的输入规模，不是端到端进程实测，不能作为线上内存数字。

## 根因链

触发：大工具结果或长历史会话触发 Reason 投影 / Micro Compact / 工具分派
→ 渲染入口先物化全部可见消息（深拷贝正文），再按 action 截断、替换或省略
→ 分派入口再把同一份历史复制成尾部快照，跨工具执行持有
→ 被省略或归档的正文在复制后即被丢弃，峰值内存与累计复制成本按副本数放大。

- 观察：上述代码路径与副本计数可由代码直接复核；输出语义由现有测试锚定。
- 推断：具体内存峰值与延迟倍数未做端到端测量；24 MB 夹具不能外推为线上规模。
- 平方增长仅指跨轮累计复制成本，不是常驻内存的平方增长。

## 影响面

- 用户影响：大结果、长历史会话在渲染与每轮工具批次出现额外内存峰值和复制耗时；极端情况下可能触发 OOM 或明显卡顿（未做端到端量化）。
- 受影响模块/调用点：`compact_v2/projection_archive.rs`、`compact_v2/projection.rs`、`session/transcript/entry.rs`、`stages/invocation_context.rs`、`stages/tool_dispatch.rs`、`stages/tool_dispatch/execution.rs`。
- 是否可规避：无独立开关；减少单条工具输出或历史长度只能缓解，不能消除多份副本。

## 修复方案

同一分支分两阶段；两段均已在本 worktree 实现。

### 阶段一（本 PR）：借用式投影 + 单份共享快照

- `TranscriptEntry::project_message_ref()` 返回 `Cow<BaseMessage>`：普通消息借用，canonical/legacy compact reminder 按原编码 `Owned`；`project_message()` 委托并 `into_owned()`。
- `projection_archive::render` 不再物化 `visible_model_messages()`；直接按 `!is_excluded && visible_to(Model)` 遍历 `transcript.entries()`，正文在确定输出时才复制一次。
- 先处理 `OmitToolExchange` 再复制正文：独立工具结果整条省略不构造消息；混合消息只复制存活兄弟块并保持原始 block index。`ResultReceipt` / `PreviewArchiveRead` / `CompactToolResult` / `ReplaceMedia` / `CompactText` / `Exclude` 仍在复制前完成判定；`Keep` 才复制最终内容。
- 工具配对 `pairs()` 改为借用扫描；`safe_action()`、`project_content()`、嵌套 `ToolResult` 与签名检查不再产生中间块副本。
- `InvocationContext` 只保留一份 `Arc<Vec<BaseMessage>>`：构造时追加本批 AI，记录 `history_len` 与尾部身份；`messages()` 返回提交前前缀，`messages_for_dispatch()` 在身份匹配时仅 `Arc::clone`。nested / App / Host synthetic AI 保留 owned 回退；fork 的独立子历史不变。
- 不修改 `ToolContext` 公共 API、存储布局与数据库 schema；`CompactConfig` 的既有字段不变，阶段二新增 `max_resident_history_bytes`（见下）。

### 阶段二（本 PR）：独立于 token 的驻留字节预算

- `CompactConfig` 新增 `max_resident_history_bytes: u64`，serde 默认与 `Default` 同为 128 MiB（134217728），0 表示关闭；不新增环境变量，不改数据库 schema。**0 值语义已决策为关闭门控，不是立即超限**：`ResidentHistoryPressure::is_enabled()` 要求 `limit_bytes > 0`，与 `ResidentHistoryPressure::DISABLED` 及既有调用方默认行为一致，测试/嵌入方可显式禁用而无需构造极大阈值。
- 计量口径为 `ResidentHistoryPressure::from_transcript_with_protected` 对自有区与祖先区的 `reserved` 逐条汇总；自有区按 Full 归档谓词再分为可回收/不可回收，`excluded_bytes` 是两者的子集，不重复相加。`ResidentHistoryPressure` 是只读 helper，不扩 `ContextPressure`。
- Compact 阶段在 token refresh 前采集驻留压力（超限时无需物化模型视图），绕过 token Skip 与已消费压力样本；该路径不构建 Micro facts，避免远程 `read_archive_metadata` 探测失败把驻留 Full 提前变成 `micro_failure`。`compact_v2` 在 cache-aware、Micro planner 与 Micro 达标返回之前直接复用 `run_full_or_degrade()`（`FullEscalationReason::ResidentHistoryBudgetExceeded`）。
- `context_budget` 缺失时 token 压力按 0 处理（`estimated_tokens=0`、`context_window=0`、`output_reserve=0`，不执行 token refresh），但驻留压力仍独立采集，超限时照常执行 resident Full；未超限时保持原 Skip 行为。`compact_config` 缺失时无法判断阈值、自动开关与驻留上限，阶段明确跳过并记录 `缺少 compact_config`，不伪造默认配置。
- `run_compact_with_facts` 以 `ResidentHistoryPressure::DISABLED` 兼容既有入口；该入口不只用于测试：手动 `/compact` 的 `session/exec/compact_pipeline.rs:441 → run_compact_with_protected` 也经它进入。由于 `force=true` 在驻留判断之前返回 `FullEscalationReason::ManualForce`，驻留门控实际不参与，行为无变化。
- 触发顺序保持：manual force 最优先；`auto_compact_enabled=false` 不因驻留触发；shadow 只记录拟执行 Full、不提交。`new_work_since_full` 作为同一工作单元的“强制一次”去重，避免 Full 后残余在无新工作时反复摘要。
- Full 成功但仍超限只记录 limit、own/inherited reserved、总量与残余并返回，不做硬准入；Full 失败在低 token 下也显式传播（`resident_triggered`）。
- Reason 在 `before_model`/工具目录发布后增加独立补检：hooks 前后 canonical revision 变化且驻留超限时复用一次无 hooks core，不要求投影 token 增长。
- 日志复用现有 tracing 字段，不新增事件；`FullEscalationReason` 新增枚举值只进入内部 `ObserveEvent`，ACP `ExecutorEvent::CompactCompleted` 不携带该字段，外部严格客户端解码兼容性不受影响。
- 新增 `compact_v2/resident_budget_test.rs` 与 `stages/resident_budget_test.rs`，用小阈值覆盖低 token/无 usage/已消费样本、边界、Micro 达标仍 Full 且 planner 调用为 0、未超限保留 Micro-only、Full 失败传播、Full 后残余、auto-disabled/shadow/manual；并覆盖 `new_work_since_full` 去重、archive metadata 探测失败仍执行 Full、hooks 后 canonical 变化补检（不变或未超限不触发）。

### 回归测试

- 新增 `peri-agent/src/agent/compact_v2/projection_borrowed_test.rs`：固定 `MessageId` 与完整消息列表序列化 golden，覆盖 Text/Blocks/Raw、v2/v3、reminder、预览/收据/省略、混合块、签名、Unicode 与 `is_error=true` 工具结果。goldens 已在基线 `e598d178` 的旧渲染器上重放通过（4/4 字节一致）。
- 追加 malformed Raw 对照：`malformed_raw_blocks_keep_bytes_and_degrade_projection` 以缺失字段的 text/tool_result/tool_use/reasoning/image 与未知 type 锚定 `raw_block_parses` 的 `Unknown` 退化——raw 字节原样保留、逐块 action 不生效、仅普通 text 被截断；`malformed_raw_projection_directives_are_rejected` 锚定 `safe_action` 不把畸形 raw `tool_result` 当作可投影目标。两条用例在 `/tmp` 基线渲染器上同字节/同错误通过（6/6）。
- 新增 `peri-agent/tests/projection_allocation_test.rs`：同输出尺寸、4 MiB vs 8 MiB 源正文，断言投影分配不随源正文增长。
- 复跑 `projection_render_test.rs`、`projection_archive_test.rs`、`projection_restore_test.rs`、`invocation_context_test.rs`、`tool_dispatch/execution_test.rs`、`effective_dispatcher_test.rs`。

## 验收标准

1. 所有既有投影/恢复/渲染测试输出不变；新增 golden 字节稳定。
2. 源正文翻倍时投影分配保持常量级（allocation test）。
3. 同一模型批次的两个工具共享同一 dispatch 快照（`Arc::ptr_eq`），hook 只见提交前前缀；nested synthetic AI 走 owned 回退。
4. 以下命令真实通过（结果见“验证记录”）：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- projection
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- invocation_context
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- tool_dispatch
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --test projection_allocation_test
./scripts/cargo-rmcp-patched.sh check --locked -p peri-agent
./scripts/cargo-rmcp-patched.sh fmt --all -- --check
git diff --check
```

## 验证记录

第一段实现完成后的真实结果（worktree `.worktrees/micro-compact-resident`，基线 `e598d178`）：

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- projection` → **71 passed; 0 failed**（含新增 3 个 golden 用例）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- invocation_context` → **6 passed; 0 failed**。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- tool_dispatch` → **38 passed; 0 failed**。首轮并行执行时 `test_malformed_tool_ids_log_per_call_at_default_level` 因全局日志捕获竞争失败 1 次；单独运行与重跑均通过，后续全量 lib 也通过。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --test projection_allocation_test` → **1 passed; 0 failed**（4 MiB vs 8 MiB 源正文，分配 < 1 MiB 且增长 < 64 KiB）。
- `./scripts/cargo-rmcp-patched.sh check --locked -p peri-agent` → 成功。
- `./scripts/cargo-rmcp-patched.sh fmt --all -- --check` → 成功（先执行 `fmt --all`）。
- `git diff --check` → 成功，无输出。
- 补充全量回归：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib` → **1293 passed; 0 failed; 5 ignored**。
- 补充 provider wire：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --test micro_compact_wire_test` → **4 passed**。
- 补充 middleware：`fork_test` → 12 passed；`integration_v2` → 9 passed、1 failed（`test_p0_2_background_defined_skill_preload_once_after_parent_cancel` 的 skill preload 计数）。该失败与 `spec/issues/2026-10-23-p1-subagent-memory-retention.md` 记录的既有基线失败一致，与本改动路径无关；本 session 未在独立 `e598d178` worktree 重新复现。

限制：golden 常量在借用式实现落地后捕获；与旧渲染器的字节等价由既有投影/恢复/渲染测试与全量 lib 回归间接锚定，未在独立基线 worktree 重放 golden。

### 第二阶段（2026-10-24）

worktree `.worktrees/micro-compact-resident`，基线 `e598d178` + 第一段未提交改动。

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- resident` → **17 passed; 0 failed**（含两处新增测试文件）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- compact` → **273 passed; 0 failed; 3 ignored**；其中 `test_ten_thousand_exchanges_have_bounded_index_and_net_projection`（10,000 Read 夹具）通过，未改成端到端内存验收。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib` → **1310 passed; 0 failed; 5 ignored**。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- full_commit_releases_archived_own_bodies_and_container_capacity` → **1 passed; 0 failed**（Full 后正文容量下降）。注意按模块名 `transcript_memory_probe` 过滤为 0 tests，实际测试名如上。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- compact` → **14 passed; 0 failed**。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- config` → **72 passed; 0 failed**。
- `./scripts/cargo-rmcp-patched.sh check --locked -p peri-agent -p peri-acp-types -p peri-tui` → 成功。
- `./scripts/cargo-rmcp-patched.sh fmt --all -- --check` → 成功（先执行 `fmt --all`）。
- `git diff --check` → 成功，无输出；`git status --short` 仅包含本 issue 范围内文件与第一段既有改动。

### 独立验证后的修复与复核（2026-10-24）

- **Reasoning 省略语义**：`projection.rs::omitted_block` 的 `Reasoning` 条件由错误的 `remaining_calls > 0` 改为与基线 retain 一致的 `remaining_calls == 0`。修复前 v3 golden 固化了“全部调用被删仍保留签名思考”的错误输出；修复后该块按基线删除。
- **基线 golden 对照**：复制 worktree 到 `/tmp`，仅用 `git show e598d178:...` 替换 `projection.rs` 与 `projection_archive.rs`，并把 golden 测试挂载到基线模块；测试文件仅补 `ToolCallRequest` 导入。基线渲染器未改动，`borrowed_tests` **4 passed; 0 failed**（3 个原 golden + `is_error` golden）。
- **驻留路径探测顺序**：`stages/compact.rs` 在 `resident_exceeded` 时不再调用 `micro_facts::snapshot`，避免远程 archive metadata 失败把 Full 提前变成 `micro_failure`。
- **新增测试**：`resident_full_is_not_repeated_without_new_work`、`resident_full_does_not_probe_archive_metadata`、`reason_rechecks_resident_after_hooks_canonical_change`、`reason_skips_resident_recheck_without_canonical_change`、`reason_skips_resident_recheck_below_limit`、`error_tool_results_keep_their_projection_bytes`、`omitted_exchange_does_not_copy_the_source_body`、`pairing_scan_borrows_canonical_bodies`。
- **变异验证**：
  - 删除 `new_work_since_full` 条件 → `resident_full_is_not_repeated_without_new_work` 失败（“无新 Human/Tool 时不得重复驻留 Full”）。
  - 删除 `|| (canonical_changed && resident_exceeded)` → `reason_rechecks_resident_after_hooks_canonical_change` 失败（compact 调用 0 != 1）。
  - 恢复无条件 `micro_facts::snapshot` → `resident_full_does_not_probe_archive_metadata` 失败（`archive capability probe failed`）。
- **命令结果**：`--lib -- resident` **22 passed**；`--lib -- compact` **274 passed; 3 ignored**；`--lib` **1316 passed; 5 ignored**；`--test projection_allocation_test` **3 passed**；`check -p peri-agent -p peri-acp-types -p peri-tui` 成功；`fmt --all -- --check` 成功；`git diff --check` 无输出。
- **分配测试口径**：catalog 元数据另受 64 KiB 上限约束，分配测试用相同 member id 的小载荷构造 catalog，只测渲染器是否复制 canonical 正文；1 MiB / 2 MiB 源正文下分配仍为常量级。同一测试在基线渲染器上 3/3 失败（分别分配 4,200,714 / 2,644,541 / 1,069,295 字节，均超过 1 MiB 上限），证明旧式预复制会被该上限捕获。

### 复审阻断项修复（2026-10-24）

- **STD-SIZE-001**：`compact_v2/projection.rs` 从 1320 行拆分出 `compact_v2/projection_render.rs`：`RenderAction`、`project_message_ref`、`MessageDraft`、`project_content_ref`、`project_tool_result_content_ref`、`omitted_block`、raw/head-tail helper 及仅被这些使用的构造逻辑移入 sibling 模块；`projection.rs` 保留编排、恢复、估算与对外入口，`projection_archive` 显式引用 `render`。拆分后 `projection.rs` **627 行**、`projection_render.rs` **714 行**，均低于 1000 行；未删注释或压缩格式，goldens 与既有测试字节不变。
- **context_budget 缺失**：`stages/compact.rs:75` 仅在 `compact_config` 缺失时跳过并记录；`context_budget` 缺失时 `stages/compact.rs:109-135` 将 token 压力置 0、跳过 token refresh，但驻留压力照常采集，超限时执行 resident Full；未超限保持 Skip。新增 `resident_budget_without_context_budget_still_full` 覆盖超限 Full 一次与未超限不调用模型两种行为。
- **malformed Raw 对照**：新增 `malformed_raw_blocks_keep_bytes_and_degrade_projection` 与 `malformed_raw_projection_directives_are_rejected`，分别锚定 `raw_block_parses` 的 `Unknown` 退化（raw 原样保留、逐块 action 不生效、仅普通 text 截断）和 `safe_action` 拒绝畸形 raw `tool_result`。在 `/tmp/peri-baseline-golden`（基线 `e598d178` 渲染器）复跑 **6 passed**，与当前渲染器字节/错误一致。
- **0 值语义**：已决策为关闭门控（不是立即超限），理由与实现见 `docs/design/micro-compact.md` 第 2 节与 `planner.rs::ResidentHistoryPressure::is_enabled()`；实现与测试保持 `limit_bytes > 0` 才启用。
- **测试稳定性**：两个 `resident_budget_test.rs` 的 `capture_logs` 在进程内安装一次 sink 全局 subscriber，避免并行测试 drop thread-local guard 时重建 callsite interest、使日志断言偶发丢事件。修复后 `--lib -- resident` 连续 10 次 **23 passed**。
- **复审命令结果**：`check-file-size.sh` exit 1，仅列出 9 个既有超限文件（源码 3 / 测试 6），本次修改与新增文件均不在列表；基线 golden 副本 **6 passed**；`--lib -- resident` **23 passed**；`--lib -- compact` **276 passed; 3 ignored**；`--lib` **1319 passed; 5 ignored**；`--test projection_allocation_test` **3 passed**；`check -p peri-agent -p peri-acp-types -p peri-tui` 成功；`fmt --all -- --check`、`git diff --check` 成功。

### CI 专属失败修复（2026-10-24）

- **现象**：PR #285 的 macOS `Build` 在 peri-agent lib 测试步骤失败，`malformed_raw_blocks_keep_bytes_and_degrade_projection` 报告键序不一致；本地单包测试全部通过。
- **根因**：CI 以 `--workspace` 构建，feature 统一启用 `serde_json/preserve_order`，`serde_json::Value` 的对象改为插入序；golden 常量按默认 BTreeMap 键序书写，因此只在单包构建下匹配。
- **修复**：`assert_golden` 解析两侧 JSON 后比较 `serde_json::Value` 结构，投影语义不依赖键序；6 个 golden 用例在两种 feature 投影下均通过。
- **验证**：`cargo test --locked --workspace --lib --no-run` 后用生成的 `peri_agent` 测试二进制跑全量 lib 套件 → **1319 passed; 0 failed; 5 ignored**（workspace feature 投影）。
- **无关失败**：同次 Linux job 的 `peri-js-runtime` 3 个 host lifecycle 用例等待 READY 超时，该 crate 不在本改动范围；新 run 若再次出现按环境抖动重跑并记录。

### 残余主导门控修复（2026-10-24，含独立复核整改）

- **问题**：预算按 `own + inherited` 计量，但 Full 只归档自有区（skip ancestor）且保留 System/protected 后台结果。不可回收部分本身 ≥ 上限时，Full 后总量仍超限；此后任何新增非空 Human/Tool 消息都让 `new_work_since_full` 成立，下一轮再次 Full。内存不降，子 Agent 反复支付摘要延迟与模型成本。
- **计量**：`is_full_archivable_entry` 成为 Full 归档谓词的单一权威（非 excluded、非 System、非 protected）；Full 与 `ResidentHistoryPressure` 共用。压力拆为 `reclaimable_own_reserved_bytes`、`irreducible_own_reserved_bytes`（System/protected）与 `irreducible_inherited_reserved_bytes`，日志与残余记录分别输出三类字节及总量。已 excluded 的非 System 条目会在 Full 提交后由 `shrink_after_full_commit` 释放，不作为稳定残余。
- **门控**：`CompactBudgetRecovery` 记录每次**实际应用** Full（含 `InterruptedAfterCommit`；shadow/失败/未提交不记录）后的压力快照，并新增 `resident_gate_state().decide(...)`：
  - 未超限 → 不 Full；`limit_bytes == 0` 仍关闭门控。
  - 上次 Full 后快照按当前 limit 仍超限（残余主导）→ 只有**可回收 own 相对上次 Full 后的增长**严格超过完整 limit（`current.reclaimable_own > snapshot.reclaimable_own + limit`）**且**出现新工作时才允许下一次 Full；System/protected/继承区不参与该阈值，连续小工具结果与同一工作单元均不重复触发（摘要自身即使 > limit 也不触发）。
  - 其他 → 维持「总量超限且存在新工作」。
- **快照失效**：快照绑定记录时的 limit、transcript 实例身份、watermark 条目 id 与其内容指纹（kind + 正文长度 + 稳定哈希）。指纹覆盖 watermark 的同 id 替换；`shrink_after_full_commit` 或 rewind 删除 watermark 时按 id 查不到，同样失效。limit 变化、实例替换、watermark 替换/删除时回到普通门控；追加与非 watermark 条目替换不影响快照。内容 revision 每次追加都会变化，不能作为失效依据。
- **锁顺序**：transcript → `budget_recovery`，无反向路径。Compact 与 Reason 共用 `resident_gate`；gate 所需事实（pressure、snapshot 有效性、new_work/watermark）先在各自 guard 内取出并释放，再做纯判定/记录。记录快照同样先读 transcript 再锁状态；代码中不存在 transcript guard 存活时锁 `budget_recovery` 的路径。该设计以「同一任务串行、无并发 record」为前提；若未来并发修改 transcript 或记录快照，需要重新评估两个快照的时点一致性。
- **回归**：
  - `resident_inherited_residual_does_not_repeat_full_for_small_new_work`（继承区 > limit，首次 Full 后连续 5 轮小工具结果不再 Full）
  - `resident_full_resumes_after_own_region_exceeds_limit`（可回收 own > limit 后恢复 Full）
  - `resident_irreducible_own_does_not_repeat_full_for_small_new_work`（System 不可回收 own > limit，连续小工具结果不再 Full；可回收 own > limit 后恢复）
  - `resident_below_inherited_limit_keeps_new_work_gate`（继承区 < limit 时仍按原逻辑触发）
  - `resident_shadow_does_not_record_pressure_snapshot`（shadow 不更新快照）
  - `resident_snapshot_invalidates_when_limit_changes`、`resident_snapshot_invalidates_when_transcript_replaced`、`resident_snapshot_invalidates_on_watermark_replacement`（limit/实例/watermark 替换使快照失效）
  - `resident_snapshot_survives_non_watermark_replacement`（非 watermark 同 id 同内容替换后追加小结果不重新 Full）
  - `resident_summary_growth_over_limit_does_not_repeat_full`（Full 后摘要自身 > limit，连续小结果不再 Full；相对增长 > limit 后恢复）
  - `resident_interrupted_after_commit_records_pressure_snapshot`（已提交后取消仍记录快照）
  - `resident_pressure_separates_system_and_protected_own`（System/protected 不计入可回收）
- **变异验证**：把残余阈值改回「全部 own > limit」，`resident_irreducible_own_...` 在第 0 轮失败；把增长阈值改回绝对 `reclaimable > limit`，`resident_summary_growth_over_limit_...` 在第 0 轮失败；移除 watermark 指纹比较，`resident_snapshot_invalidates_on_watermark_replacement` 失败；把指纹改为覆盖全部条目（任意条目变更失效），`resident_snapshot_survives_non_watermark_replacement` 失败。恢复后通过。
- **验证**：`--lib -- resident` **35 passed**；`--lib -- compact` **277 passed; 3 ignored**；`--lib` **1331 passed; 0 failed; 5 ignored**；`check -p peri-agent -p peri-acp-types`、`fmt --all -- --check`、`git diff --check` 成功；`check-file-size.sh` 仅列出既有 9 个超限文件。

**结论**：修复与验证完成，已提交 PR #285，待合并。

## 残余风险

1. **Full 后仍超限只报告**：不做硬准入或终止运行；进程可能在仍高于预算的驻留下继续请求，仅保留日志与残余事实。
2. **snapshot 是容量下界**：只统计 `String::capacity` / `Vec::capacity`，不含分配器元数据、共享节点、HashMap 桶与页粒度；实际 RSS 可能更高。
3. **计量范围有限**：只覆盖热 transcript 的 own/inherited 区，不包含投影、分派、nested/fork 的短期副本；这些副本仍可能在同一时段叠加峰值。
4. **未做 RSS 端到端实测**：本 issue 不提供线上内存数字，`planner_test` 的 24 MB 夹具仍只是单元输入规模。
5. **工作单元去重语义**：`new_work_since_full` 只把非空 Human/Tool 消息视为新工作；仅有 reminder 或 AI 文本增量时不会因驻留重复 Full（与 token Full 的“新工作”定义一致）。
6. **枚举兼容性**：`ResidentHistoryBudgetExceeded` 只新增在内部 `ObserveEvent` 路径；ACP `ExecutorEvent::CompactCompleted` 不携带 `full_escalation_reason`，外部严格客户端 wire 不受影响。若未来把该枚举直接暴露给外部协议，旧严格解码器会拒绝新值。
7. **不可回收 own 与 inherited 无法被 Full 缩减**：Full 保留 System、受保护后台结果与祖先区；这些部分本身超过上限时 Full 无法把总量降回预算。残余主导门控会阻止连续小工作反复 Full，但不提供淘汰策略。快照锚定 watermark 条目的 id + 内容指纹；非 watermark 条目替换不会失效，这是有意的保守取舍，可能让增长基线略偏保守（少触发一次 Full）。删除 watermark 条目（shrink/rewind）与实例替换仍会失效。
8. **`compact_llm` 缺失**：低 token 且驻留超限时会进入 Full 并可能返回 `CompactNoLlm`；这是驻留门控绕过 token 阈值后的显式失败，不会静默跳过。
9. **Reason 每轮快照成本**：Reason 的补检每轮调用 `ResidentHistoryPressure::from_transcript_with_protected`，需遍历条目并汇总容量；相对模型请求延迟很小，但历史极长时是可观测的 CPU 成本。
10. **锁顺序**：transcript 与 `budget_recovery` 不嵌套；gate 所需事实（pressure、snapshot 有效性、new_work/watermark）先在各自 guard 内取出并释放，再做纯判定/记录，无反向路径。若未来新增持锁回调，需重新审查。
