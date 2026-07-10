> 归档于 2026-07-10，原路径 spec/issues/2026-07-06-panels-selection-no-scroll-follow.md

# 面板选中项超出可见行后看不到（缺 scroll 跟随）

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-06

## 问题描述

侧栏面板（Model / Mcp / Plugin / Hooks / Tasks / Cron / Memory / Betas / ThreadBrowser / Agent / Workflow 等）的列表项支持 Up/Down 切换选中项、`>` 光标渲染，但当列表项数量超过面板可见行数时，选中项的 `>` 行会滚出视野——用户无法看到当前选中了哪一项。

Slash command popup（`slash_completion.rs`）已经实现了「选中项保持在上 1/3 处可见」的滚动跟随，面板没有这个能力。

用户希望了解：**ratatui-kit 是否提供了可直接复用的「选中项跟随可见」能力**，以避免每个面板手写一遍 scroll 跟随逻辑。

## 症状详情

| 维度 | 表现 |
|------|------|
| 选中机制 | ✅ 各面板有 `selected = hooks.use_state(\|\| 0usize)` + Up/Down 修改 + `>` cursor 渲染 |
| 选中项视觉 | ✅ 在可见区域内，选中行有 `>` 前缀和高亮样式 |
| 列表超屏时 | ❌ 选中项滚到可见区域外后，`>` 消失；用户按键 Up/Down 不知道当前选中了哪一项 |
| Slash popup 对比 | ✅ `slash_completion.rs:183-197` 计算 `scroll_start = sel_idx - visible_rows/3`，让选中项保持在上 1/3 处 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 打开任一侧栏面板（如 Cron、Memory、ThreadBrowser）
  2. 让面板中的列表项数量超过面板可见行数（例如 cron 任务多、memory 文件多、历史 thread 多）
  3. 按 Up/Down 反复移动选中项
  4. 观察选中项的 `>` 前缀
- **期望**：选中项 `>` 始终可见（自动滚动跟随）
- **实际**：选中项超出可见区域后 `>` 消失，无法判断当前位置
- **环境**：所有终端 / 所有 OS

## 涉及文件

**症状所在（面板列表渲染处）**：

- `peri-tui/src/kit/panels/mcp.rs` —— Mcp 面板，`selected = use_state(0)` + `>` cursor
- `peri-tui/src/kit/panels/plugin.rs` —— Plugin 面板，同模式
- `peri-tui/src/kit/panels/hooks.rs` —— Hooks 面板，同模式
- `peri-tui/src/kit/panels/tasks.rs` —— Tasks 面板，同模式（含 SubAgent 双区域）
- `peri-tui/src/kit/panels/cron.rs` —— Cron 面板，同模式
- `peri-tui/src/kit/panels/memory.rs` —— Memory 面板，同模式
- `peri-tui/src/kit/panels/betas.rs` —— Betas 面板，同模式
- `peri-tui/src/kit/panels/thread_browser.rs` —— Thread 浏览面板，同模式
- `peri-tui/src/kit/panels/agent.rs` —— Agent 面板，同模式（双区域）
- `peri-tui/src/kit/panels/workflow.rs` —— Workflow 面板，同模式
- `peri-tui/src/kit/panels/model.rs` —— Model 面板，`selected_tab` + `✔` 渲染

**用户参考的「正面示例」**：

- `peri-tui/src/kit/slash_completion.rs:183-197` —— Slash popup 中已实现的 scroll_start 跟随逻辑（手写）

**用户询问的 ratatui-kit 能力（已初步调研）**：

- `ratatui-kit` 源码路径：`~/.cargo/git/checkouts/ratatui-kit-57880b1120009d67/45b9b3a/crates/ratatui-kit/src/components/`
- `components/select.rs` —— 提供 `Select` 组件，基于 ratatui `List` + `ListState`，**理论上自带 offset 跟随选中项**（ratatui `List` 标准行为）。当前面板**未使用**该组件。
- `components/scroll_view/state.rs` —— `ScrollView::State` 仅有 `scroll_to_top` / `scroll_to_bottom`，**没有** `ensure_visible(index)` 或 `scroll_to_index`。
- `components/list_state.rs` —— 仅是 `ListState` 的 thin wrapper（同步 default selection），不含 scroll 跟随。
- `components/virtual_list.rs` —— 虚拟列表，可能相关但需进一步评估。

## 期望改进方向

用户期望：

1. 侧栏面板的列表选中项在超出可见行后能保持可见（类似 slash popup 的「上 1/3 处可见」行为）
2. 优先复用 ratatui-kit 已有能力（如 `Select` 组件），避免每个面板手写 scroll 跟随
3. 若 ratatui-kit 现有能力不足，决定是「上游 PR 给 ratatui-kit 添加 `ensure_visible`」还是「peri-tui 内部抽象一个 list-with-follow 工具」

> **注**：此处仅记录用户期望的方向选择，具体方案设计属于 fix-issue / brainstorming 阶段，不在本 issue 范围内。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-06 | — | Open | agent | 创建（issue-create skill 访谈生成） |
| 2026-07-06 | Open | Fixed | agent | 推广 scroll_start_for_selected 辅助函数到 6 个面板 |

## 修复记录

### 修复 #1（2026-07-06）

- **操作人**：agent
- **用户原意**：选中项移出可见区域时，`>` 光标能跟随可见（仿 slash_completion 的「上 1/3 处可见」行为）
- **方案选择**：放弃 ratatui-kit `Select` 组件（硬编码边框 + 整行 highlight 损失视觉），放弃自定义 `ListView` 组件（4 次失败，stateful+layout+嵌套存在未知陷阱）。改为算法辅助函数复用——保留各面板原有渲染逻辑，只在循环加 `skip(scroll_start).take(VISIBLE_ITEMS)`
- **修复内容**：
  1. `peri-tui/src/kit/list_nav.rs`：新增 `scroll_start_for_selected(selected, item_count, visible_items) -> usize` 辅助函数 + 单测（验证「上 1/3 处可见」语义 + 边界处理）
  2. `plugin.rs`：内联视口跟随（试点），VISIBLE_ITEMS=3（每项 4 行，items area 12 行）
  3. `hooks.rs`：VISIBLE_ITEMS=3（每项 3 行——matcher 缺失时加占位空行）
  4. `mcp.rs`：VISIBLE_ITEMS=6（每项 2 行，items area 12 行）
  5. `memory.rs`：VISIBLE_ITEMS=13（每项 1 行，items area 13 行）
  6. `cron.rs`：VISIBLE_ITEMS=4（每项 3 行——next_fire 缺失时加占位空行）
  7. `thread_browser.rs`：VISIBLE_ITEMS=4（每项 3 行，items area 12 行）
- **未推广**：`tasks.rs` / `agent.rs`（section 混合模式，selected 索引和视觉行不直接对应，需 section-aware 算法，未来如需要再单独处理）；`workflow.rs`（项数固定 4 个，不会超屏）
- **验证状态**：待验证（用户已确认 plugin.rs 试点生效；推广面板待手动验证）
