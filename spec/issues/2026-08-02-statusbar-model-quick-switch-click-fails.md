# 运行多轮次后点击状态栏模型段无法弹出模型快速切换弹窗

**状态**：Open
**优先级**：中
**创建日期**：2026-08-02

## 问题描述

TUI 运行多轮 agent 对话、运行时间较长之后，鼠标点击状态栏（Row1）的模型段（alias/model 文本）不再弹出模型快速切换小弹窗（ModelQuickSwitchPopup）。预期是点击模型段文本弹出锚定在模型段上方的四档切换弹窗；实际点击无任何反应。

## 症状详情

- 早期（刚启动 / 内容少时）点击模型段正常弹出弹窗；运行若干轮后点击失效。
- 失效后键盘切换（Ctrl+T 循环模型）仍正常，说明仅鼠标点击路径受影响。
- 失效表现为"点击无反应"，而非弹窗闪现后关闭。
- 排查观察（与代码行为对照，待对抗验证）：
  - 状态栏 Row1 的 spans 固定顺序为 `模式 · cwd · 模型段 · CPU · MEM · bg任务 · ctx使用率`，MEM 无条件存在，模型段**永远不是最后一个 span**。
  - 多轮运行后尾部内容累积（`CONTEXT_USAGE` 出现、后台任务计数、CPU>50% 显示），状态栏总宽度可能超过终端宽度 → `needs_wrap` 翻转、Row1 折行为双行。
  - 折行模拟逻辑在循环结束后取 `line_idx`（最后一个 span 所在行）作为模型段所在行；折行点落在模型段之后时，模型文本实际渲染在第 0 行而点击判定用第 1 行，点击区域整体错位一行。
  - 次要观察：折行模拟按 span 粒度近似，而 ratatui `Paragraph::wrap` 按字符/单词粒度折行，模型段 span 内部（含空格）可能被真实渲染拆分跨行，模拟与渲染存在行/列偏差。

## 复现条件

- **复现频率**：偶发（条件满足后持续失效）
- **触发步骤**：
  1. 启动 TUI（终端宽度较窄更容易触发，如 ≤100 列）
  2. 连续运行多轮 agent 对话，使状态栏尾部出现 ctx 使用率 / 后台任务计数 / CPU>50% 显示
  3. 状态栏内容总宽度超过终端宽度，Row1 进入双行折行
  4. 点击模型段文本 → 弹窗不弹出
- **环境**：macOS；终端尺寸较窄时更容易复现；与权限模式/模型名长度无关

## 涉及文件

- `peri-tui/src/kit/status_bar.rs` —— 状态栏 Row1 组件；模型段点击区域由折行模拟计算（`line_idx`/`click_start`/`click_end`），点击判定用 `mouse.row == area.y + line_idx`
- `peri-tui/src/kit/status_bar_test.rs` —— 现有测试仅覆盖辅助函数，未覆盖折行模拟的点击区域计算
- `peri-tui/src/kit/popups/model_quick_switch.rs` —— 弹窗组件（仅消费 `MODEL_SWITCH_ANCHOR`，与本 issue 的触发端相关）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建 |
| 2026-08-02 | Open | Resolved | agent | 修复完成：词级折行模拟 + ground-truth 验证（28 测试全过，clippy clean，code-review APPROVED） |

## 修复记录

### 修复 #1（2026-08-02）

- **操作人**：agent
- **用户原意**：多轮运行（状态栏折行为双行）后，点击状态栏模型段仍能正常弹出模型快速切换弹窗。
- **修复内容**：
  - `peri-tui/src/kit/status_bar.rs`：`model_click_areas` 从 span 粒度重写为**词级折行模拟**（对齐 ratatui-widgets 0.3.2 reflow.rs `Wrap{trim:false}` 语义）——词流扫描（跨 span 边界合并词、空白累积为词前宽度 ws）、每个词一个点击区域 `(line_idx, x_start, x_end)`，模型段跨行/尾部折行时每行各有区域。根因（折行点落在模型段之后时旧实现用最后一个 span 的行号判定点击）消除。
  - 换行边界经对抗审查修正为 WordWrapper 逐字符增量检查的精确等价形式：`line_x + ws + w - cw_last >= area_w`（cw_last = 词尾字符宽，修复词尾 CJK 宽字符 + 超界 1 列时的点击失效残留）；含行尾空白回填丢弃（L139-150 语义）。
  - `peri-tui/src/kit/status_bar_test.rs`：测试矩阵更新 + 新增 9 个测试（含 4 个 TestBackend ground-truth 渲染对比，逐位断言词首/词末字符位置）。
- **涉及 commit**：未提交（工作区改动）
- **验证状态**：已验证——`cargo test -p peri-tui --lib -- status_bar` 28 passed；`cargo test -p peri-tui --lib` 694 passed；`cargo clippy -p peri-tui --all-targets -- -D warnings` clean；第二轮 code-review APPROVED（3000 例随机差分验证模拟与真实渲染逐位一致）。

## 独立缺陷记录（本 issue 修复过程中发现，另行处理）

以下缺陷与本次修复无因果关系，按 devflow plan 要求在此登记，单独开 issue 处理：

1. **AskUser 面板残留**：AskUser 交互面板关闭后存在残留（具体表现见 devflow explore 阶段记录），属弹窗生命周期管理问题。
2. **`push_popup_kind` 无条件覆盖竞态**：Hitl/OAuth 路径的 `push_popup_kind` 调用无条件覆盖当前 popup kind，多弹窗并发场景下可能互相顶掉（竞态），需改为有条件推送或加锁。
