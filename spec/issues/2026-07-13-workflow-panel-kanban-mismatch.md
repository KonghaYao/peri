# Workflow Panel 看板形态未按 TUI-PAGE.md 6.14 实现

**状态**：Open
**Triage**：ready-for-agent
**优先级**：高
**分类**：Bug / Design Gap
**创建日期**：2026-07-13

---

## Problem Statement

Workflow Panel（`peri-tui/src/kit/panels/workflow.rs`）当前是一个**静态信息型面板**，展示固定行（engine、binary、subagent count、self-check）和操作说明文本，完全不看 workflow 运行时数据。这与 `TUI-PAGE.md` §6.14 定义的设计严重不符。

### 当前实现 vs 设计对比

| 维度 | 设计 (TUI-PAGE.md §6.14) | 当前实现 | 差距 |
|------|--------------------------|---------|------|
| **顶部 Tabs** | 多个 workflow run 可切换，状态 emoji 前缀 | 无 | 完全缺失 |
| **左右分栏** | Phase 列（左 2）+ Agents 列（右 8） | 单列扁平列表 | 完全缺失 |
| **Phase 展示** | 按顺序展示 Design / Build / Verify / Ship，状态 emoji | 无 | 完全缺失 |
| **Agent 展示** | name + model + token 用量 + 工具调用数 | 无 | 完全缺失 |
| **状态标识** | ✓ ● ○ ✗ 四态 emoji | 无 | 完全缺失 |
| **详情区** | 明确不要 Selected Phase / Selected Agent 详情 | 无（也无详情） | 无需变更 |
| **快捷键** | Tab/Shift+Tab 切 run，←→ 切 pane，↑↓ 导航，Enter inspect | ↑↓导航 + Enter/Esc 关闭 | 缺失 Tab 切 run、←→ 切 pane |
| **数据源** | ACP-only: `peri/unstable-event` → `workflow-snapshot` | VIEW_MODELS → subagent count | 完全未对接 |

### 代码证据

当前 `peri-tui/src/kit/panels/workflow.rs:26-167`：

- **第 32 行**：从 `VIEW_MODELS` 派生 subagent group 数量，这只是间接的"活跃度提示"，不是真正的 workflow 数据
- **第 45-62 行**：`rows` 是硬编码的静态 (label, value) 对，没有 workflow run 数据
- **第 74-89 行**：事件处理只有 Up/Down/Enter/Esc，没有 Tab/Shift+Tab/←→
- **第 151-162 行**：渲染是单列 `Paragraph`，没有 tabs bar 和左右分栏

## Solution

将 Workflow Panel 从静态信息页改造为符合 §6.14 设计的**看板型 workflow run 查看器**。

### 1. 数据接入

- 从 ACP 事件流接入 `peri/unstable-event` 中的 `workflow-snapshot` 事件
- Payload 定义为 `WorkflowRunListDto`，扩展 phase/agent 运行态信息
- 写入全局 Atom（如 `WORKFLOW_SNAPSHOT_ATOM`），Workflow Panel 订阅渲染

### 2. 布局改造

- **顶部 Tab Bar**：横向排列所有 workflow run，每个 tab 显示 `{status_emoji} {run_id}`，当前选中高亮
  - Tab 数量 > 3 时支持 Tab/Shift+Tab 循环切换
- **主体左右分栏**（宽 2:8 比例）：
  - 左栏：Phase 列表（Design / Build / Verify / Ship ...），每项显示状态 emoji + phase 名称
  - 右栏：Agents 列表，每行显示 agent name、model、token 用量、工具调用数
    - 可以有多 agent 属于同一 phase（如 smoke-test 跟 coder-1 同属 Build）
- **Footer**：统一快捷键提示条（i18n）

### 3. 状态 Emoji 映射

| 含义 | Emoji |
|------|-------|
| 运行中 | ● |
| 已完成 | ✓ |
| 待执行 | ○ |
| 失败 | ✗ |

列表中**只显示 emoji**，不要重复显示英文状态文本。

### 4. 快捷键

- `Tab`：下一个 workflow run
- `Shift+Tab`：上一个 workflow run
- `← / →`：在 Phase 列 / Agents 列之间切换焦点
- `↑ / ↓`：在当前焦点列内上下导航
- `Enter`：进入选中项详情（inspect）
- `Esc`：关闭面板

### 5. 不做

- **不**显示 "Selected Phase" / "Selected Agent" 详情区（§6.14 明确禁止重复信息）

## User Stories

1. 作为 TUI 用户，我想在 Workflow Panel 看到当前 session 中所有已启动的 workflow run，通过顶部 tab 切换查看
2. 作为 TUI 用户，我想每个 workflow run 的各 phase（Design/Build/Verify/Ship）执行状态一目了然，通过 emoji 快速区分已完成/运行中/待执行/失败
3. 作为 TUI 用户，我想看到每个 agent 的模型、token 用量和工具调用数，以便评估 workflow 执行情况
4. 作为 TUI 用户，我想用 Tab/Shift+Tab 快速切换 workflow run，用 ←→ 键在 Phase 和 Agents 栏之间切换
5. 作为 TUI 用户，Workflow Panel 应该实时反映 workflow 状态变化（运行中 → 完成/失败），而非静态文本

## Affected Files

- `TUI-PAGE.md`：§6.14 为参考设计，不修改
- `peri-tui/src/kit/panels/workflow.rs`：**主改造文件**——从静态信息型面板重写为看板型
- `peri-tui/src/kit/atoms.rs`：新增 `WORKFLOW_SNAPSHOT_ATOM` 全局状态
- `peri-tui/src/kit/acp_events.rs` / `acp_notifier.rs`：接入 `workflow-snapshot` 事件
- `peri-tui/src/kit/acp_types.rs`：新增 `WorkflowRunListDto` / phase/agent 类型定义
- `peri-tui/locales/en/main.ftl` / `locales/zh-CN/main.ftl`：新增面板内快捷键提示等 i18n key

## Open Questions

1. `peri/unstable-event` 中的 `workflow-snapshot` 事件是否已有完整 payload 定义？还是需要先对接 `@peri-workflow` npm CLI 的输出协议？
2. Tab 数量超过终端宽度时，是否需要左右滚动箭头，还是仅通过 Tab/Shift+Tab 切换（当前 spec 未涉及溢出）？
3. Phase 列表是否固定为 Design/Build/Verify/Ship，还是需要从 workflow run 动态读取？
4. Agent 在右栏中的排序方式——是否按 phase 分组展示？例如 Build phase 下的 agent 显示在该 phase 区域右侧？

## Acceptance Criteria

- [ ] Workflow Panel 不再显示静态信息（engine/binary/self-check 等）
- [ ] 顶部 Tab Bar 能展示多个 workflow run，Tab/Shift+Tab 可切换
- [ ] 主体为 Phase（左）/ Agents（右）双栏布局
- [ ] Phase 和 Agent 的状态均通过 ●/✓/○/✗ emoji 显示
- [ ] Agent 行展示 name、model、token 用量、工具调用数
- [ ] 快捷键 Tab/Shift+Tab/←/→/↑/↓/Enter/Esc 均生效
- [ ] 数据来自 ACP 事件流（peri/unstable-event → workflow-snapshot），非静态硬编码
- [ ] 无 Selected Phase/Agent 详情区
