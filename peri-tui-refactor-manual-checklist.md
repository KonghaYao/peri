# peri-tui 重构手工检查清单

提交：`515b94e9 refactor tui kit panels and overlays`

本清单用于手工验证本次 peri-tui kit 重构后的交互、布局与渲染行为。

## 启动方式

```bash
cargo run -p peri-tui -- -a
```

## 1. Panel / Popup 基础交互

### Panel 显示

逐个打开以下 panel，确认能正常显示：

- [ ] Model
- [ ] Config
- [ ] Status
- [ ] Tasks
- [ ] Agents
- [ ] Memory
- [ ] MCP
- [ ] Hooks
- [ ] Plugins
- [ ] Cron
- [ ] Workflow
- [ ] Betas
- [ ] Login
- [ ] Threads

每个 panel 打开时检查：

- [ ] 输入区仍固定在底部
- [ ] 消息区不白屏
- [ ] panel 没有被明显裁切
- [ ] panel 宽度/居中效果符合预期
- [ ] panel 关闭后主界面恢复正常

### Esc 优先级

- [ ] 有 popup 时，`Esc` 优先关闭 popup
- [ ] 无 popup 但有 panel 时，`Esc` 关闭 panel
- [ ] 无 popup / panel 时，`Esc` 不触发异常状态

## 2. 消息区滚动

重点验证本轮修复过的键盘滚动回归：

- [ ] 鼠标滚轮能滚动消息区
- [ ] `Ctrl+Up` 能向上滚动消息区
- [ ] `Ctrl+Down` 能向下滚动消息区
- [ ] `Ctrl+Home` 能跳到消息区顶部
- [ ] `Ctrl+End` 能跳到消息区底部
- [ ] 普通 `Up/Down` 在输入区多行编辑时仍用于光标移动/历史 fallback
- [ ] 普通 `Up/Down` 不应被消息区抢走

## 3. Slash / Mention 补全

### Slash completion

- [ ] 输入 `/` 能弹出 slash completion
- [ ] 输入 `//` 不导致 CPU 飙高、卡死或持续重绘
- [ ] slash 列表能显示可用命令
- [ ] skills / available commands 能按预期出现在 slash 列表中

### Slash command → panel 映射

验证常用命令能打开对应 panel：

- [ ] `/model`
- [ ] `/config`
- [ ] `/status`
- [ ] `/tasks`
- [ ] `/agents`
- [ ] `/memory`
- [ ] `/mcp`
- [ ] `/hooks`
- [ ] `/plugins`
- [ ] `/cron`
- [ ] `/workflow`
- [ ] `/betas`
- [ ] `/login`
- [ ] `/threads`

### Mention completion

- [ ] 输入 `@` 能弹出文件 mention
- [ ] mention 列表可上下移动选择
- [ ] 选中文件后能插入输入区
- [ ] mention popup 关闭后焦点回到输入区

## 4. Popup 场景

触发并检查以下 popup：

- [ ] HITL approval popup
- [ ] OAuth popup
- [ ] AskUser popup
- [ ] Rewind popup

每个 popup 检查：

- [ ] popup 不白屏
- [ ] popup 打开时 panel/input 焦点不乱
- [ ] `Esc` 能关闭 popup
- [ ] `Enter` 行为符合预期
- [ ] popup 关闭后主界面恢复正常

### AskUserPopup 特别检查

当前代码已确认 AskUserPopup 会展示真实问题 payload，但 `Enter` 似乎只关闭 popup，是否应提交答案尚未确认。

重点验证：

- [ ] AskUserPopup 中显示的问题与 agent 实际问题一致
- [ ] 选择选项后 UI 状态正确更新
- [ ] 按 `Enter` 后是否真的把答案提交回 agent
- [ ] 如果只关闭 popup、不提交答案，记录为独立问题跟进

## 5. 极小终端 / 重绘稳定性

把终端窗口缩小后反复操作：

- [ ] 开关 panel
- [ ] 开关 popup
- [ ] 触发 redraw
- [ ] 切 session（如果可用）
- [ ] 切 thread（如果可用）

观察是否出现：

- [ ] 整屏白屏
- [ ] 主界面被挤没
- [ ] 输入区消失
- [ ] panel/popup 残影
- [ ] 内容高度异常抖动

## 6. View rendering 文案/颜色

跑一个真实 agent turn，尽量覆盖以下消息类型：

- [ ] user message
- [ ] assistant message
- [ ] reasoning
- [ ] tool running
- [ ] tool success
- [ ] tool error
- [ ] diff hidden
- [ ] diff shown
- [ ] subagent running
- [ ] subagent done
- [ ] system note

检查：

- [ ] 颜色符合预期
- [ ] 中文/emoji 文案可接受
- [ ] 终端窄宽度下不明显错位
- [ ] tool card 仍可读
- [ ] diff card 仍可读
- [ ] subagent group 展开/折叠状态正常

## 7. 提交后工作区状态

提交后当前工作区预期只剩未跟踪 `spec/`：

```text
?? spec/
```

这两个 issue 文档此前建议不要混入本次 peri-tui refactor 提交。如需保留，应单独确认并单独提交。

## 已完成的自动验证

提交前已通过：

```bash
cargo check -p peri-tui
cargo clippy -p peri-tui --lib
cargo test -p peri-tui --lib
lefthook run pre-commit
```

结果摘要：

- `peri-tui --lib` 全量：**316 passed**
- `cargo check -p peri-tui`：通过
- `cargo clippy -p peri-tui --lib`：通过（warnings 均为既有代码）

---

## 8. StatusBar 模型名称字段（2026-07-04 修复）

- [ ] 状态栏第一行显示 `provider/model_name` 格式（如 `anthropic/claude-sonnet-4-20250514`），而非短别名 `sonnet`
- [ ] provider/model 显示为整体统一样式（之前是 provider/muted + /dim + alias/text 三段分离）
- [ ] SetupWizard 页面的 provider 信息也显示完整模型名
- [ ] 模型切换时（Ctrl+T / /model 面板）状态栏实时更新

## 9. 输入历史（2026-07-04 新增）

- [ ] 提交消息后关闭重启 Peri，之前的历史依然存在（`~/.peri/input-history.json` 持久化）
- [ ] ↑ 浏览历史，首次进入历史模式时当前输入文本被保存为草稿
- [ ] 浏览到最旧位置后 ↓ 返回编辑态，草稿自动恢复
- [ ] 纯空白文本（仅空格）不保存为草稿
- [ ] 连续输入相同文本不入栈（去重）
- [ ] 历史容量上限 1000 条

## 10. 预测输入（2026-07-04 新增）

- [ ] Agent 回复完成后，输入区下方出现灰色预测文本（需等待 ACP 下发 `prediction_ready` 事件）
- [ ] 输入区为空 + 有预测文本时，`Tab` 接受预测并注入到输入框
- [ ] 打印任意字符后预测文本消失
- [ ] `Enter` 提交消息后预测文本消失

## 11. macOS Option 键兼容层（2026-07-04 新增）

- [ ] macOS 终端：`Alt+M` 可循环切换模型（等价 `Ctrl+T`）
- [ ] macOS 终端：`Alt+Shift+M` 可循环切换 Provider（等价 `Ctrl+Shift+T`）
- [ ] 标准终端：`Ctrl+T` / `Ctrl+Shift+T` 行为不变

## 12. @mention 模糊匹配（2026-07-04 优化）

- [ ] 输入 `@` 弹出文件候选，输入模糊关键词能匹配到非前缀命中的文件（如 `@ipua` 匹配 `input_area.rs`）
- [ ] 结果按相关度降序排列
- [ ] 空 prefix 时显示前 20 个文件
- [ ] ↑/↓ 选择，Enter 插入

## 13. InputArea 编辑快捷键（2026-07-04 新增/确认）

- [ ] `Shift+Enter` / `Alt+Enter` 插入换行（多行编辑）
- [ ] `Ctrl+W` 删除光标前一个词
- [ ] `Ctrl+Backspace` 删除前一个词（与 `Ctrl+W` 等价）
- [ ] `Ctrl+Delete` 删除光标后一个词
- [ ] `Alt+←/→` 按词跳转光标
- [ ] `Home` / `End` 跳转到行首/行尾
- [ ] `Ctrl+U` 从光标位置删除到行首（textarea 有内容时）；消息区向上翻页（textarea 为空时）
- [ ] `Ctrl+D` 消息区向下翻页
- [ ] `Ctrl+C`：有文本时清空；loading 中打断 Agent；空闲 +2s 内双击退出
- [ ] `Esc`：关闭 @mention/slash popup；双击打开 Rewind 选择器
- [ ] `Ctrl+T` 循环切换模型（opus → sonnet → haiku）
- [ ] `Ctrl+Shift+T` 循环切换 Provider
- [ ] `Shift+Tab` 循环切换权限模式
