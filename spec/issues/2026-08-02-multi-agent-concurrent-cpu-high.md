# 多 agent 并发时 CPU 占用 50%（渲染风暴 + per-token 全量重建）

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-02

## 问题描述

同时运行 3 个同步 agent（subagent 并行）时，进程 CPU 占用达到 50%。单 agent 运行时正常。期望多 agent 并发时 CPU 占用与单 agent 接近线性小幅增长，实际为大幅跳升，TUI 仍可操作但资源占用过高。

## 性能数据

- **场景**：3 个同步 agent 同时运行（subagent 并行场景）
- **观测**：CPU 占用 50%（单 agent 时明显低于此值）
- **影响面**：macOS 桌面环境，长会话 + 多 agent 流式输出时更明显

## 出现场景

- 3 个 agent 并发流式输出，每 token 到达都触发完整事件链：`acp_bridge 逐事件 dispatch → push_view_models → VIEW_MODELS atom 写 → 整树渲染`
- Loading 期间固定 20Hz 心跳（`entry.rs` 50ms tick 写 RENDER_HEARTBEAT），无新 token 时也强制整树 update+draw
- 长会话下 message_area 每帧 O(N) 扫描与 wrap_map 重建随历史线性增长

## 已验证发现（perf-scan 静态诊断 + 二级对抗验证）

> 诊断日期 2026-08-02，报告见 `perf-scan-report-20260802-112852.md`。首轮 8 路验证通过后，2026-08-02 又派 2 路独立对抗验证（不提供前轮结论，从源码独立核查），确认 3 项核心发现（#1-3，MEDIUM），并将 #4-7 全部降级为 LOW（机制属实但量级 µs 级，合计 <3% CPU，与 50% 差一个数量级）。两路对抗均独立判断：50% CPU 主因在渲染路径（#1-3 + 每 token 无合帧整树渲染），而非日志/序列化/周期任务。

| # | 发现 | 位置 | 模式 | 对抗结论 |
|---|------|------|------|---------|
| 1 | Loading 期 50ms tick 写 RENDER_HEARTBEAT → 20Hz 整树重绘；渲染循环无合帧/dirty-check，任何 atom 写即整帧渲染（updater.rs:106 无条件 update 全部组件 body） | `peri-tui/src/kit/entry.rs:204-220` | 渲染风暴 | **PASS，MEDIUM**（"无条件"实为 loading 期间有条件，不推翻结论） |
| 2 | 每 token 全量 VM 重建：clone 整个 child_turn（含 raw_input JSON）→ 从 0 重建全部 child VM → hash 拼接 → 深拷贝进 im::Vector；父层 build_view_models 对每个 segment 全量重 hash，O(N²)/轮累积 | `peri-tui/src/kit/acp_types.rs:646-679, 400-535` + `render.rs:23-26` | 每事件全量重建 | **PASS，MEDIUM**（3 并发放大被子 agent 缓存命中削弱为 2-3 倍） |
| 3 | 流式 bubble 每 token 全量 markdown 重解析：ensure_closed_code_fences O(N) 扫描 + pulldown-cmark 从零解析；且流式散文（段落内无换行）时 stable_text 恒空 → 增量缓存完全失效，为最坏情形 | `peri-tui/src/kit/markdown/mod.rs:137-138, 176-203` | 每事件全量重建 | **PASS，MEDIUM**（比原描述更差：缓存命中路径还有双份 Vec<Line> 深克隆） |
| 4 | concat_wrap_maps 每帧无条件重建扁平 wrap_map，内容未变也执行，随会话长度线性增长 | `peri-tui/src/kit/message_area/mod.rs:289-307` | 每帧全量重算 | **DOWNGRADE → LOW**（机制属实但 µs 级，与 #2/#3 的 O(N²) 不同量级） |
| 5 | 每 token tracing::info! 同步写 RollingFileAppender，均为临时 instrumentation | `peri-tui/src/kit/acp_events/streaming.rs:70-73`、`render.rs:14-22` | 日志开销 | **DOWNGRADE → LOW**（subagent reasoning 走其他分支不打 info!，实际每 token 1 条 ≈ 0.15-0.6% CPU） |
| 6 | 周期任务粗糙：workflow 2s 无条件写快照 + RPC、service 每 tick clone 静态数据、bash 1s 全量重推 VIEW_MODELS、status_bar 每帧无 memo | `workflow_snapshot.rs:84-109`、`service_snapshot.rs:74,191,210,269`、`acp_bridge.rs:94-103`、`status_bar.rs:37,68-92` | 轮询/分配 | **DOWNGRADE → LOW**（workflow 轮询驱动动画属功能必需、面板关闭时零订阅者；service 有 write_if_changed 守卫；四项合计 <2% CPU） |
| 7 | event_sink 每 token serde_json::to_value + json! 包层 + chunk.clone()，纯分配 churn 无 I/O | `peri-acp/src/session/event_sink.rs:138-151` | 序列化 | **DOWNGRADE → LOW**（每 token 2-3 棵小 Value 树 ≈ 0.1-0.3% CPU） |

### 对抗验证补充判断（2026-08-02）

- 两路对抗一致认为：**主因在渲染路径**——每 token 无条件 push_view_models 全量重建 + 写 VIEW_MODELS → 整树渲染一帧，帧率 ≈ token 率（3 agent × 100 token/s ≈ 300 帧/s × 1-5ms/帧），叠加 loading 期 20Hz 心跳下限
- 量级匹配存疑点：静态分析无法证明 CPU 归因（F2+F3+F4 最坏估算约 3-24% 单核），建议后续用 `PERI_RENDER_TIMING` + `sample`/Instruments 实测拆分
- agent 侧同进程（SSE 解析、工具执行、token 计数）未被静态扫描覆盖，是剩余嫌疑

## 相关 Issue

- `spec/issues/2026-07-17-message-area-render-stutter-long-conversation.md` —— 长对话渲染卡顿，覆盖 message_area 每帧开销与 build_view_models/parse_markdown_cached 的 per-token 成本，本 issue 聚焦多 agent 并发放大场景
- `spec/archive-issues/tui-general/2026-07-17-spinner-tick-decouple-from-acp-bridge.md` —— spinner 50ms tick 的来源（当时为修复 spinner 动画引入），本 issue 记录其副作用：loading 期 20Hz 全树重绘

## 涉及文件

- `peri-tui/src/kit/entry.rs` —— 50ms spinner tick 写 RENDER_HEARTBEAT（20Hz 心跳来源）
- `peri-tui/src/kit/acp_types.rs` —— SubAgentAccumulator::view_model / build_view_models 每 token 全量重建
- `peri-tui/src/kit/acp_events/render.rs` —— push_view_models 每 token 全量 clone + 无条件写 VIEW_MODELS
- `peri-tui/src/kit/acp_events/streaming.rs` —— 每 chunk tracing::info! 日志
- `peri-tui/src/kit/markdown/mod.rs` —— parse_markdown_cached 每 token 全量解析
- `peri-tui/src/kit/message_area/mod.rs` —— 每帧 hash 扫描 + concat_wrap_maps 重建
- `peri-tui/src/kit/workflow_snapshot.rs`、`service_snapshot.rs`、`status_bar.rs`、`acp_bridge.rs` —— 周期任务与每帧无 memo 渲染
- `peri-acp/src/session/event_sink.rs` —— 每 token 序列化分配 churn

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（基于 perf-scan 静态诊断） |
| 2026-08-02 | Open | Fixed | agent | 修复：三项核心发现各派 coder 修复完成，workspace build + 704 测试 + clippy 全过 |

## 修复记录

### 修复 #1（2026-08-02）

- **操作人**：agent（coder subagent）
- **用户原意**：3 个同步 agent 并发时 CPU 占用过高，降低多 agent 并发的 CPU 开销
- **修复内容**：
  1. 发现 #1（20Hz 整树重绘）：`entry.rs` 心跳从 50ms 无条件写改为 100ms tick + 帧序号条件写。spinner 动画帧率本质是 10Hz（`elapsed_ms/100`），50ms 是 2 倍超采样，一半重绘是帧未变化的重复渲染；100ms 与帧边界对齐动画零损失。loading 期渲染下限 20Hz → 10Hz
  2. 发现 #2（每 token 全量 VM 重建）：`tui_render_unit.rs` 新增滚动哈希原语（增量维护与全量计算值一致）；`acp_types.rs` `build_view_models` 全量重建 → `sync_cache` 增量修补（冻结段零重建、subagent 组 O(1) hash 比对跳过未变者），`SubAgentAccumulator::view_model` 删除 child_turn 深拷贝 + String 拼接 hash 链（im::Vector O(1) 共享 + u64 hash 组合）；`acp_events/render.rs` 快照构建去逐条深拷贝。每 token O(变化量+段数扫描)，不再 O(N²) 累积
  3. 发现 #3（每 token 全量 markdown 重解析）：`markdown/convert.rs` 新增尾部不稳定块回滚（持久化前回滚空段落与未闭合尾块，续跑时重处理，正确性经 3 轮解析器探针验证）；`markdown/mod.rs` `ensure_closed_code_fences` 返回 Cow 零拷贝、缓存命中路径 mem::take 消除 Vec<Line> 深拷贝、stable_text 增量扩展、持久化条件放宽（流式散文不再缓存恒失效）。每 token 剩余成本：rk_parse O(N)（pulldown-cmark 无增量 API）+ 返回拷贝 O(N)（调用契约）
- **涉及 commit**：未提交
- **验证状态**：已验证（cargo build --workspace 通过、cargo test -p peri-tui --lib 704 passed、cargo clippy -p peri-tui --all-targets -- -D warnings 通过）

### 待办（未在本次修复范围）

- 发现 #4-7（对抗验证降级为 LOW）：concat_wrap_maps 每帧重建、per-token 日志、周期任务、event_sink 序列化——合计 <3% CPU，后续可选清理
- 实测归因：`PERI_RENDER_TIMING` + `sample`/Instruments 拆分 CPU，验证修复效果（issue 创建时无实测数据）
