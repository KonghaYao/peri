# P0：TUI 流式与渲染链冗余

**状态**：Astra 源码复核完成；首批修复、F2 共享气泡与 F3 已闭合 fence 分片已实施，复杂可变尾部与资源现场验收继续推进。下文发现位置保留审计时快照，实施状态以「裁决与实施」为准。
**优先级**：P0（用户明确要求将核实正确项提升）。这表示实施优先级，不表示已证明现场资源事故因果；独立于 [`2026-10-06-p0-dev-peri-high-cpu-memory.md`](2026-10-06-p0-dev-peri-high-cpu-memory.md) 的现场验收。
**类型**：TUI 渲染计算冗余 / 常驻内存放大 / 日志噪音。
**来源**：主 agent 直接复核 + 三组只读 subagent（渲染层、常驻内存、流式事件链）。全部为源码静态阅读，未运行 perf / heap / 现场采样。

## 背景与范围

- 只覆盖 `peri-tui` 在**普通文本流式渲染**上的成本，分三个粒度：每 chunk、每次 publication（≤20/s）、每帧。
- 不含 peri-agent 侧 WorkState 读写放大（见 P0 文件），不含 subagent 显示身份的配对问题（见 `2026-10-06-p0-subagent-display-identity.md`）。
- 判定口径：`ui.streaming_mode` 未配置 = `Streaming`（默认）；`Block` / `None` 为非默认分支，单列。
- 事实与推断分列：源码位置与机制为代码事实；复杂度与"常驻倍数"是结构推导，未实测；现场是否命中某分支标注为未知。

## 一、已排除：默认流式主路径不存在 per-chunk O(n)

| # | 机制 | 位置（快照） | 结论 |
| --- | --- | --- | --- |
| E1 | 文本 / 推理追加 | `kit/acp_types/current_turn/streaming.rs:76-82, 90-104` | 仅 `push_str` + 增量滚动哈希 + `invalidate_cache()`，O(chunk) |
| E2 | 每 chunk 的 `push_acp_state` | `kit/acp_events/render.rs:754-768` | 快照值不变时不写回、不 wake；`view_count` 为 O(1) |
| E3 | 分帧发布 | `kit/acp_bridge.rs:257, 269-346, 510-523` | `Deferred` 置 `pending_deadline`（不延期）+ 事件循环 deadline 分支 → 50ms 合帧 |
| E4 | 增量 VM 缓存 | `kit/acp_types/current_turn/projection.rs`（`sync_cache` 长度门控与冻结段单建） | 冻结段只建一次，只有 trailing 重建 |
| E5 | 历史折叠缓存 | `kit/acp_events/fold.rs`（源未变即 O(1) 短路） | 每 chunk 不重跑历史 |
| E6 | 工具分组切点复用 | `kit/acp_events/render.rs`（`GroupCut` / `stable_cut`） | 只重建变化后缀 |

已修 P0（`../history/2026-09.md` 2026-09-27 条目）的机制在当前代码中成立，本文件不重复登记。

## 二、发现清单

每条给出：位置 / 事实 / 触发条件 / 成本 / 证据强度 / 修复方向 / 验收要点。

### F1 后台 subagent 实时明细：逐 chunk 全文深拷贝 + 全文哈希 + atom 写唤醒

- **位置**：`kit/bg_task_live.rs:187-215`（文本）、`:217-247`（推理）、`:20-32`（写入口）。
- **事实**：每个 chunk 取 `BG_LIVE_DETAIL` 写锁（DerefMut 触发 wake → 渲染一帧），`b.clone()` 深拷贝整段 bubble 文本后 `push_str`，再 `recompute_hash()`；后者对全文做滚动哈希。注册点在 `kit/acp_events/subagent.rs:29-31`（仅后台 subagent 进入 `BG_AGENT_IDS`，同步 subagent 不进入）。
- **触发条件**：存在后台 subagent 且正在产出文本/推理 chunk。
- **成本**：Θ(n) per chunk，n 递增时为 Θ(n·m)；且逐 chunk 唤醒渲染，绕过 50ms 合帧。
- **证据强度**：主 agent 复核（`bg_task_live.rs`、`subagent.rs:29-31`、`atoms.rs:753-760` 已读）。**现场是否处于后台 subagent 运行未验证**；实际 chunk 数与字节量未测。
- **修复方向**：见 §四.1（保留并修 / 移除该实时明细 / 整体移除后台 subagent 能力）。
- **验收要点**：面板显示仍完整（`subagent_detail_test.rs` 既有回归）；修复前后同负载对比发布频率与哈希调用次数。

### F2 每次 publication 重建 trailing owned String，叠加 `im` 按 chunk COW

- **位置**：`kit/acp_types/current_turn/projection.rs:300-336`（`text_slice.to_string()` 与冻结分支）、`:62-64`（reasoning `to_string()`）；`kit/acp_events/render.rs:382-396`（`join_into`，注释自证"由 im 在推入时按 chunk 承担 COW"）。
- **事实**：trailing 长度变化或待冻结即重建 owned bubble；`cached_view_models.set(...)` 与 `join_into` 对与旧快照共享的向量写入，触发 im chunk 级深拷贝。
- **触发条件**：每次 publication（流式中 ≤20/s）。
- **成本**：每次发布 ≥2×n 字节分配 + 若干 chunk 深拷贝；chunk 边界与拷贝量为推断。
- **证据强度**：`projection.rs:300-336` 与 `render.rs:363-396` 主 agent 复核；COW 深拷贝规模为推断。
- **修复方向**：trailing 用共享（`Arc<str>`）或延迟物化；避免对共享 `im` 向量直接 `set` / `push_back`。
- **验收要点**：发布频次不变时，"每次发布复制字节数"随 n 的斜率下降（需 §五.3 计量）。

### F3 尾部 markdown 每次发布重解析；stable 冻结在若干语法前停住

- **位置**：`kit/markdown/mod.rs:193`（每次 `input.starts_with(cache.chunk_source)` 前缀比较）、`:209-231`（`stable_chunk_end` 之后 tail 整段 `parse_markdown_piece`）、`:131-178`（`stable_chunk_end` 规则）。
- **事实**：`stable_chunk_end` 遇到 `!`、`[`+`]`、列表、表格、未闭合 fence 即停止前进；此后每次发布 tail 近似全文，重复全量解析与重新物化。
- **触发条件**：流式文本处于上述任一未完结构（长代码块、长列表、含链接的行）。
- **成本**：O(tail ≤ n) × 发布频率。
- **证据强度**：`mod.rs:186-231` 主 agent 复核；`stable_chunk_end` 早退规则来自 subagent 报告，未逐行复核。
- **修复方向**：尾部解析结果按内容哈希分片缓存；记录"上次停点"避免重复全扫。

### F4 常驻多份拷贝；历史消息冻结后仍保留 `chunk_source` 原文副本

- **位置**：`kit/markdown/mod.rs:245-249, 266-268`（冻结后仍 `push_str` 原文进 `chunk_source`）、`kit/markdown/mod.rs` 的 `MarkdownRenderCache` 字段；`kit/message_area/vm_cache.rs:94-123`（`Arc::new(lines.clone())` + `build_wrap_map`）、`:176-215`（`VmCacheSlot`）。
- **事实**：同一文本在 VM 正文、`chunk_source`、`stable_chunk_blocks`、`stable_chunks`、`MarkdownLineCache.stable`、`VmCacheSlot.lines` 多处常驻，且多为 owned。
- **成本**：单条流式消息 ≈4–6×n（推导，未实测）；历史消息 ≈2–3× 会话总文本。
- **证据强度**：结构与字段为代码事实；倍数为结构推导。
- **修复方向**：冻结消息不再保留 `chunk_source`；历史渲染缓存改为有界（可见窗口 / LRU）。
- **验收要点**：同会话历史下 RSS 随消息数增长的斜率；`vmmap -summary` / `heap` 对比。

### F5 每帧 O(N) 分配与 running VM 100ms 强制重建；每次发布全快照差异扫描

- **位置**：`kit/message_area/mod.rs:250-308`（每帧 `running_flags` + `item_hashes` 两个 Vec 与 `rebuild_indices`；`:299-300` running 且 `anim_frame` 变化即强制重建，anim_frame 粒度 100ms）、`kit/acp_events/render.rs:363-370`（`first_divergence` 每发布 O(N)）。
- **成本**：O(N) 每帧 + O(N) 每发布；与当前文本长度无关，与历史条数相关。
- **证据强度**：两处均由主 agent 复核。
- **备注**：P0 文件已把"发布 O(N) 全扫"记为保留的必要全量工作；本文件只把"每帧两份 Vec 分配"与"running 强制重建"登记为可疑冗余。

### F6 Block 模式逐 chunk `char_indices().nth()`（非默认分支）

- **位置**：`kit/acp_events/mod.rs:82-86`、`kit/acp_events/streaming.rs:200-201`。
- **事实**：字符偏移换算字节偏移用 `char_indices().nth(since_chars)`；命中边界时再做 `text.chars().count()`。
- **触发条件**：`ui.streaming_mode = "block"`；默认 `Streaming` 不触发。
- **成本**：O(n) per chunk（0 次发布也照付）。
- **证据强度**：subagent 报告，主 agent 未逐行复核。
- **修复方向**：维护字节游标（追加时同步推进）。

### F7 生产 bridge 遗留 `[CLEAR_DEBUG]` info 日志

- **位置**：`kit/acp_bridge.rs:607-637`。
- **事实**：`/clear` 诊断 instrumentation 仍在事件循环内；`is_dirty != was_dirty` 即 `tracing::info!`。流式中发布清 `cache_dirty`、下个 chunk 又置位 → 频率 ≈ 发布频率（~20 条/s）。
- **证据强度**：主 agent 复核。
- **修复方向**：移除，或降级 `debug` 并只在 `just_reset` 时输出。

### F8 运行中工具卡每次发布重建，哈希对输出全文 `format!`

- **位置**：`kit/acp_types/current_turn/projection.rs`（`TurnSegment::Tool` 运行中每 sync 重建）、`kit/tui_render_unit/tool_card.rs:99-122`。
- **事实**：`recompute_hash` 把 `input_summary` / `output_summary` / `presentation` 等拼进一个 `format!` 再哈希。
- **成本**：每次发布 O(Σ 运行中卡输出大小)；大 diff / 长 stdout 时显著。
- **证据强度**：`tool_card.rs:99-122` 主 agent 复核；"运行中每 sync 重建"来自 subagent 报告。
- **修复方向**：运行中卡的哈希按增量（输出追加）维护，避免全文 `format!`。

### F9 高亮缓存：命中仍深拷贝、miss 存两份

- **位置**：`kit/markdown/code_block.rs:33-100`。
- **事实**：`HlCache` cap 32；命中时调用方 `(*arc).clone()` 深拷贝 `Vec<Line>`（`:91-92`）；miss 时 `result` 与 `arc_result` 各存一份（`:96-99`）；LRU 用 `Vec::retain` + `push`（O(cap)）；缓存为全局静态、跨会话不清。
- **证据强度**：主 agent 复核。
- **修复方向**：命中返回 `Arc` 不深拷贝；改字节上限；会话边界清理。

### F10 面板每帧全量重解析；死代码

- **位置**：`kit/panels/subagent_detail.rs:93-97`（每次渲染 `MarkdownRenderCache::default()`，对该组全部 VM 重新 `vm_to_lines_cached`，缓存零复用）；`kit/markdown/mod.rs:366-535`（`parse_markdown_cached` / `stable_text` / `ConvertState` 无生产调用）。
- **证据强度**：`subagent_detail.rs:93-97` 主 agent 复核；死代码来自 subagent 报告。
- **修复方向**：面板复用持久缓存；确认无 test-only 依赖后删除死代码。

## 三、关于「/bg」与"后台"能力的澄清

原流式链调查把 F1 记为"`/bg` 后台 subagent 通道"。经全仓核对，**当前工作树不存在 `/bg` 斜杠命令的实现**：

- 全仓字面量 `/bg` 只命中注释（`peri-agent/src/session/subagent.rs:5`、`directives.rs:27`、`spec/issues/2026-10-06-error-path-logging-gaps-p1.md:74`）与 `</bg_fork_directive>` 标签；
- `peri-tui` 斜杠命令面（`kit/submit_request.rs:103-119`）无 `bg` 条目；
- ACP 内置命令注册（`peri-acp/src/session/command/mod.rs:123-160`）只有 `core:compact` / `core:clear` / `core:rewind` / `core:loop`；
- `mcp-packages/workspace/src/resources/builtin/skills/` 无 `bg` 技能，故"移除这个 skill"在本仓库无对应物可删；
- `/bg` 只作为语义名出现在文档（`docs/meta-harness.md:122` "…/ Workflow agent 链 / /bg 后台 agent"）。

现存"后台"能力至少三种，**需分开裁决**：

1. **后台 subagent**：模型经 SubAgent 工具 `run_in_background: true` 发起（`peri-middlewares/src/subagent/tool/execute_bg.rs`），完成经 `peri-agent/src/session/bg_complete.rs` 通知；装配层语义为 `SubagentRunMode::Background` + `ForkDirectiveKind::Bg`。
2. **后台 shell 任务**：Bash `run_in_background` → TaskManager（`BG_TASKS`），面板 `shell_detail`，ACP 面 `session/bg-tasks` / `session/cancel-bg-task`。
3. **TUI 显示层**：`BG_AGENT_IDS`（`kit/atoms.rs:753`）与 `BG_LIVE_DETAIL`（`kit/atoms.rs:759`）。**F1 的成本出在这一层**，由 (1) 的 chunk 驱动。

因此"彻底移除 `/bg`"与"移除后台 subagent 能力"是不同范围：前者在本仓库无实现可删（若用户指的是某个外部客户端/SDK 的 `/bg` 入口，需要指明）；后者横跨 `peri-middlewares`（工具面）、`peri-agent`（装配与完成通知）、`peri-acp` / 协议与 e2e，属产品级删除，需要单独裁决与验收。本次未执行任何删除。

## 四、裁决与实施

2026-10-06 用户指定的 [P0 TUI 架构优化](2026-10-06-p0-tui-architecture-optimization.md)
已定稿并独占承接 F4 的重型历史缓存预算、F5 的增量索引/动画分离、F10 的共享缓存及详情聚合余项。
本 issue 保留已实施记录、原现场验收与其他余项；迁出工作不在此维护第二套实施清单。

保留后台 subagent 能力及实时详情，不删除工具或协议能力。Astra 核实各成本点仍存在，同时纠正以下论据：普通连续流式合帧为 50ms，Immediate/barrier 不受此上限约束；atom 通知不等于逐 chunk 必画一帧；F2 的固定「≥2×n」、F4 的常驻倍数均未被分配计量证明；运行中工具输出为空且不解析 diff，F8 不能归因为长 stdout/diff；ConvertState 为生产转换使用，不能整体删除。

| 条目 | 优先级 / 实施状态 | 当前边界与验收 |
| --- | --- | --- |
| F1 | P0 / 已实施 | 独占 BgStream 累积 + 增量 hash；50ms 独立后台 deadline，不逐 chunk 写发布 atom；工具/任务终态/receiver close flush，重置失效。publication 仍物化完整快照，未宣称消除其 O(n)。 |
| F2 | P0 / 冗余 COW 已修，必要物化保留 | assistant payload 改为 Arc；im 快照克隆、拼接和节点 COW 共享完整正文/推理，fold/终态只在确有变化时显式复制。BgStream 仍独占缓冲，避免退回逐 chunk COW。source→VM owned trailing 仍每次增长物化 O(n)，未宣称完全消除。 |
| F3 | P0 / 已闭合 fence 已分片，复杂尾部保留 | 单次逐行扫描识别 backtick/tilde fence，跨内部空行冻结已闭合代码块，后续增长只解析/物化后缀，stable rendered chunk 共享 Arc；图片尾部恢复全局字节偏移。保留引用链接后向解析、列表、表格、未闭合 fence 的保守尾部及 terminal full parse；其增长仍 O(n)，前缀一致性比较亦未消除。 |
| F4 | P0 / 确定部分已实施 | terminal 丢弃 chunk_source buffer（不只是 clear），wrap cache 接收 owned lines 后直接移动进 Arc，取消额外深拷贝。重型历史缓存预算余项迁至架构 P0 工作包 C。 |
| F5 | P0 / 确定部分已实施 | 三组逐帧扫描缓冲复用；reasoning 仅秒级时长刷新，不再强制 100ms 重建；工具/subagent 保留动画。增量索引与动画 chrome/content 分离余项迁至架构 P0 工作包 A。 |
| F6 | P0 / 已实施 | 字节发布游标，扫描新增 chunk 并有限回看跨 chunk 标记，保留 Unicode 正文和分段边界。非默认 Block 模式专项回归。 |
| F7 | P0 / 已实施 | 删除 CLEAR_DEBUG 临时 info instrumentation 与专用 helper。 |
| F8 | P0 / 已实施 | 未变工具卡不按 publication 重新构建/hash；按秒更新时间并保留 fold；流式 format writer 直接 hash，不分配完整拼接 String。 |
| F9 | P0 / 已实施 | 高亮命中返回 Arc，miss 只保存一份；32 条与 4 MiB 高亮 payload 双预算，超大结果不入缓存。最终加背景/前缀的输出仍需要一次物化；有界跨会话复用保留。 |
| F10 | P0 / 确定部分已实施 | 详情面板逐 VM 持久缓存，绑定 occurrence/内容/宽度/主题/语言/动画；旧 parse_markdown_cached 与旧字段只编入测试，生产 ConvertState 保留。共享缓存与详情聚合余项迁至架构 P0 工作包 B；测试 oracle 的最终删除/迁移仍归本 issue。 |

行为/缓存回归不替代 §五 的现场 CPU/RSS、复制与解析计量。本 issue 在余项与现场验收完成前保持 active，不标记整体已解决。

## 五、验证方式（修复前可先做，均只读）

1. `PERI_RENDER_TIMING=1`（既有埋点，`kit/message_area/vm_cache.rs`）：观察 `hash+detect` / `concat` / `frame-total` 随 n 的变化。
2. 现场栈与内存口径：`sample <pid>` / `vmmap -summary <pid>` / `heap <pid>`；注意面板 MEM 为整进程 RSS（`app/service_registry.rs`），macOS 无 jemalloc 计数/回收。
3. `PerfCounter`（`kit/acp_bridge.rs:16-57`：`ProjectionCopiedBytes` / `TailParsedBytes` / `ToolHashBytes` 等）当前受 `#[cfg(test)]` 限制，生产不可用；若要量化"每发布 O(n)"，需改为 release 可用的轻量原子计数（只计数、不记内容）。
4. 新增的进程内存告警（target `peri.mem`，>200 MiB 记 warning）可作为现场触发点的日志锚，见 P0 文件「进程内存告警」一节。

## 六、验收限制

- F3 阶段：同一隔离 worktree 验证 Markdown 定向 110 passed / 1 ignored，最终串行全量 1590 passed / 6 ignored；doc tests 编译通过（0 个示例）。128 个含内部空行的代码行块冻结后，32 次逐字增长只解析各次后缀长度之和，物化 32 行，稳定正文 Arc identity 不变；1024 组内部空行的边界搜索只遍历一次行序列。逐字符 Unicode / 引用 / 图片 / 列表 / 表格后缀在 24、80 列下比较完整参考的语义段落，terminal 输出严格相等；闭合、resize、改写与主题失效有回归。Astra 复核补齐了 tab/裸列表 marker 的保守阻断，以及内外层正文前景/代码背景缓存 key；颜色变化从 parsed blocks 重物化，不重解析。
- fence 补全仍沿用三反引号行计数，四反引号与 tilde 内部反引号存在既有语义局限；新边界保留 parity 门控，不将补全文本错误固化。完整参考也经过该补全函数，因此等价回归不代表已经证明 CommonMark fence 语义完整正确。未闭合或语义敏感尾部的 O(n) 成本仍为 P0 余项。
- F2 阶段：隔离 worktree（提交锁文件 + 本阶段 patch，不含并行任务 WIP）验证 1583 passed / 6 ignored；doc tests 编译通过（0 个示例）。128 个大气泡的快照 COW 回归记录正文/推理 Clone 为 0 字节；20 次增长发布的 source→VM 物化恰为各 trailing 长度之和，未变 trailing 不物化。变更后的旧快照与手动 fold 独立性也有回归。源测试按渲染职责拆分，保留原有案例与 canonical 模块过滤前缀。
- F2 全量并发验证有一次 `test_bridge_reset_rehydrates_pending_compact_note_for_same_session` 的全局 ACP_STATE loading 断言失败；该例定向复查和全量串行均通过。未修改该测试或用重试推定失败归因；上述 F2 通过数字以最终串行验证为准。
- 首批回归：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib` 为 1580 passed / 6 ignored；doc tests 编译通过，0 个可执行示例。后台 scheduler 覆盖固定 deadline、隐藏主流、同会话重置后重排和重置与 receiver close 同时发生的收尾。
- 本次变更的 Rust 文件均不超过 1000 行；F2 按职责拆分后，全量 size 检查仍有 10 个既存测试文件超限（首批为 13 个），未宣称全库通过。
- 首批进展按用户授权提交，保留工作区原有、与本任务无关的改动；未关闭项继续保持 P0。
- 未做现场 perf / heap 采样，不提供 CPU/RSS 改善数字；原文倍数仅为审计假设，不作为验收事实。
- §二为历史审计原始描述，不覆盖 §四的纠正和未完成范围。
- `ProjectionCopiedBytes` 已改为 source→VM `String` 构造点的正文/推理字节；`AssistantCloneBytes` 计气泡 Clone 的正文/推理字节。二者均不等于全部 allocator/COW/RSS 测量，不据此宣称现场资源收益。
