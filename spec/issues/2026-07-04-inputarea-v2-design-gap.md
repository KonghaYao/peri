# InputArea 设计规范 v2 补全——对照 TUI-PAGE 第 3 节

**状态**：Open
**优先级**：中
**创建日期**：2026-07-04

## 问题描述

TUI-PAGE.md 第 3 节（InputArea）已更新为详细设计规范，但当前 `peri-tui/src/kit/` 中部分能力尚未实现或不完整。需逐项对齐。

## 症状详情

### 已对齐的能力

| 能力 | 实现文件 | 状态 |
|------|----------|------|
| 多行 buffer + Shift+Enter 换行 | `input_area.rs` | ✅ |
| Enter 提交 + 写入历史栈 | `input_area.rs` + `input_history.rs` | ✅ |
| 历史浏览（↑/↓） | `input_area.rs:404-419` + `input_history.rs` | ✅ |
| Ctrl+C 清空 / 打断 Agent | `input_area.rs` / `event_handlers.rs` | ✅ |
| Ctrl+U 删到行首 / 空时翻页 | `input_area.rs` | ✅ |
| Ctrl+D 翻页 | `input_area.rs` | ✅ |
| Ctrl+V 粘贴 | `input_area.rs` | ✅ |
| Ctrl+W / Ctrl+Backspace 删词 | `input_area.rs:152-163` | ✅ |
| Ctrl+Delete 删后词 | `input_area.rs:178-189` | ✅ |
| Home/End, Alt+←/→ 词跳转 | `input_area.rs:386-401, 422-428` | ✅ |
| @mention 文件弹窗 | `mention_popup.rs` + `input_area.rs` | ✅ |
| Slash completion 弹窗 | `slash_completion.rs` + `input_area.rs` | ✅ |
| Esc 关闭浮层 / 双击 Rewind | `event_handlers.rs` + `input_area.rs` | ✅ |
| Tab 优先级链（预测→mention→slash） | `input_area.rs` | ✅ |
| 全局快捷键（Shift+Tab/Ctrl+T/Ctrl+O/Ctrl+B） | `event_handlers.rs` | ✅ |
| Prediction 后端（ACP `execute_prediction`） | `acp_server/mod.rs:173-229` | ✅ |

### 未实现或存在差距的能力

#### 1. 输入历史持久化（GAP-1）

**现象**：`input_history.rs` 仅使用 `INPUT_HISTORY` atom（in-memory VecDeque），进程重启后历史丢失。

**期望**：设计规范 3.5 要求持久化到 `~/.peri/input-history.json`（原子写入：先写 `.tmp` 再 rename）。

**涉及文件**：
- `peri-tui/src/kit/input_history.rs` —— 需新增 `load()` / `save()` 函数

#### 2. 历史模式草稿保存/恢复（GAP-2）

**现象**：进入历史模式时 `state.write().replace_all(historical)` 直接覆盖当前输入文本。用户通过 `history_down()` 回到编辑状态时，草稿丢失。

**期望**：设计规范 3.5 要求进入历史模式时自动保存当前编辑文本为草稿（`draft_input`），浏览到最旧方向或退出历史态时恢复草稿。

**涉及文件**：
- `peri-tui/src/kit/input_history.rs` —— 新增 `draft` 字段或辅助函数
- `peri-tui/src/kit/input_area.rs:409-410, 417-418` —— 草稿保存/恢复逻辑

#### 3. @mention 模糊匹配（GAP-3）

**现象**：当前 `mention_popup.rs` 使用前缀精确匹配，不支持模糊匹配。

**期望**：设计规范 3.3 要求使用 `SkimMatcherV2` 进行模糊匹配（如输入 `@ipua` 匹配到 `input_area.rs`）。

**涉及文件**：
- `peri-tui/src/kit/mention_popup.rs` —— 替换匹配逻辑
- `peri-tui/Cargo.toml` —— 是否已包含 skim 依赖待确认

#### 4. Slash completion 缺少 Skills 条目（GAP-4）

**现象**：已有 issue `2026-07-03-slash-popup-missing-skills.md`，slash 弹窗只显示面板命令和 ACP 内置命令，不包含已注册 skill。

**期望**：设计规范 3.4 要求三源合并（commands + skills + agent commands），按前缀精确 > 命令 > Skill > AgentCmd > 字母序排列。

**涉及文件**：
- `peri-tui/src/kit/slash_completion.rs` —— 需合并 skills 数据源
- `peri-tui/src/kit/input_area.rs` —— hook 中构建 SlashCompletionItem 时纳入 skills

#### 5. macOS Option 键兼容层（GAP-5）

**现象**：macOS 终端按下 Option 键发送合成 Unicode 字符（无修饰符标志），部分快捷键在 macOS 终端上不生效或行为异常。

**期望**：设计规范 3.9 要求 `KeyBinding` 同时匹配无修饰符的 macOS 字符路径和标准 Ctrl+字母路径。

**涉及文件**：
- `peri-tui/src/kit/event_handlers.rs` —— 新增 `KeyBinding` 结构及双重匹配逻辑
- `peri-tui/src/kit/input_area.rs` —— Option 修饰符处理

#### 6. Windows 兼容层（GAP-6）

**现象**：当前代码缺少 Windows 专用处理。

**期望**：设计规范 3.9 要求：
- IME 候选窗口定位（`Frame::set_cursor()` 跟随 textarea）
- 鼠标滚轮过滤（ConPTY 虚假 Up/Down 事件过滤）
- 模拟粘贴检测（快速按键突发转换为 Event::Paste）

**涉及文件**：
- 新文件：`peri-tui/src/kit/ime.rs`（或等效实现）
- `peri-tui/src/kit/input_area.rs` —— 集成

#### 7. 输入容量上限（GAP-7）

**现象**：历史栈当前上限为 **100** 条，设计规范 3.5 要求 **1000** 条。

**涉及文件**：
- `peri-tui/src/kit/input_history.rs:18` —— `MAX_HISTORY` 从 100 改为 1000

#### 8. 预测输入 UI 展示（GAP-8）

**现象**：`PREDICTION` atom 和 ACP 后端已就绪，但 `input_area.rs` 是否将预测文本渲染为灰色占位符待确认。

**期望**：设计规范 3.6 要求 Tab 接受预测文本、任何打印字符清除预测、提交后清除预测。

**涉及文件**：
- `peri-tui/src/kit/input_area.rs` —— 渲染预测文本 + Tab 接受逻辑

### 设计规范中定位但当前代码可能缺失的其他能力

需进一步确认的项：

| 项 | 规格能力 | 当前状态 |
|----|----------|----------|
| Ctrl+C x2 退出 | 空闲态双按 Ctrl+C ×2 in 2s 退出 | 待确认 |
| Esc loading 清缓冲 | Loading 中按 Esc 清除缓冲消息 | 待确认 |
| Delete 移除附件 | Delete 移除最近一个待上传附件 | 待确认 |
| Alt+Enter 换行 | Alt+Enter 等价 Shift+Enter 插入换行 | 待确认 |

## 涉及文件

| 文件 | GAP 编号 | 说明 |
|------|----------|------|
| `peri-tui/src/kit/input_history.rs` | GAP-1, GAP-2, GAP-7 | 持久化、草稿、容量 |
| `peri-tui/src/kit/input_area.rs` | GAP-2, GAP-8 | 草稿恢复、预测渲染 |
| `peri-tui/src/kit/mention_popup.rs` | GAP-3 | 模糊匹配 |
| `peri-tui/src/kit/slash_completion.rs` | GAP-4 | 合并 skills |
| `peri-tui/src/kit/event_handlers.rs` | GAP-5 | macOS Option 兼容 |
| `peri-tui/Cargo.toml` | GAP-3 | skim 依赖 |
| 新文件 `peri-tui/src/kit/ime.rs` 等 | GAP-6 | Windows 兼容 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-04 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
