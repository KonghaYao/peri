# peri-tui 手工验证 Checklist

> 合并自 `docs/verify-tui.md` 与 `peri-tui-refactor-manual-checklist.md`（提交 `515b94e9`）。

---

## 1. InputArea 编辑交互

- [ ] 正常中英文输入
- [ ] Enter 提交消息
- [ ] Shift+Enter / Alt+Enter 插入换行（多行编辑）
- [ ] Delete 删除字符
- [ ] Ctrl+W 删除光标前一个词
- [ ] Ctrl+Backspace 删除前一个词（等价 Ctrl+W）
- [ ] Ctrl+Delete / Alt+Delete 删除光标后一个词
- [ ] Alt+←/→ 按词跳转光标
- [ ] Home / End 跳转到行首/行尾
- [ ] 输入框多行时上下移动光标
- [ ] 超长文本正常处理
- [ ] IME 显示正常（macOS）
- [ ] Ctrl+U：有文本时删到行首；无文本时消息区向上翻页
- [ ] Ctrl+D：消息区向下翻页
- [ ] Ctrl+C：有文本时清空；loading 中打断 Agent；空闲 +2s 内双击退出
- [ ] Esc：关闭 @mention/slash popup；双击打开 Rewind 选择器
- [ ] Ctrl+T 循环切换模型
- [ ] Ctrl+Shift+T 循环切换 Provider
- [ ] Shift+Tab 循环切换权限模式

## 2. Slash / Mention 补全

- [ ] 输入 `/` 弹出 slash completion，显示可用命令和 skills
- [ ] 输入 `//` 不导致 CPU 飙高、卡死或持续重绘
- [ ] 以下命令能打开对应 panel：

| 命令 | Panel |
|------|-------|
| `/model` | Model |
| `/config` | Config |
| `/status` | Status |
| `/tasks` | Tasks |
| `/agents` | Agents |
| `/memory` | Memory |
| `/mcp` | MCP |
| `/hooks` | Hooks |
| `/plugins` | Plugins |
| `/cron` | Cron |
| `/workflow` | Workflow |
| `/betas` | Betas |
| `/login` | Login |
| `/threads` | Threads |

- [ ] 输入 `@` 弹出文件 mention
- [ ] 模糊关键词能匹配非前缀命中的文件（如 `@ipua` → `input_area.rs`）
- [ ] 结果按相关度降序排列
- [ ] 空 prefix 时显示前 20 个文件
- [ ] ↑/↓ 选择，Enter 插入
- [ ] mention popup 关闭后焦点回到输入区

## 3. 消息区显示与滚动

### 键盘滚动

- [ ] Ctrl+Up 向上滚动
- [ ] Ctrl+Down 向下滚动
- [ ] Ctrl+Home 跳到顶部
- [ ] Ctrl+End 跳到底部
- [ ] 普通 Up/Down 不被消息区抢走（用于输入区光标/历史 fallback）

### 鼠标滚动

- [ ] 鼠标滚轮能滚动消息区
- [ ] 滚动条样式正常
- [ ] 滚动条拖拽可用
- [ ] 滚动条上/下端点点击可快速跳转

### 消息类型渲染

跑一个真实 agent turn，覆盖以下类型：

- [ ] user message
- [ ] assistant message
- [ ] reasoning
- [ ] tool running
- [ ] tool success
- [ ] tool error
- [ ] Edit 工具的 diff 显示（show/hide）
- [ ] subagent running
- [ ] subagent done
- [ ] subagent 层级显示（展开/折叠正常）
- [ ] system note

验收标准：

- [ ] 颜色符合预期
- [ ] 中文/emoji 文案可接受
- [ ] 终端窄宽度下不明显错位
- [ ] tool card 仍可读
- [ ] diff card 仍可读

## 4. Panel 显示

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

每个 panel 检查：

- [ ] 输入区仍固定在底部
- [ ] 消息区不白屏
- [ ] panel 没有被明显裁切
- [ ] panel 宽度/居中效果符合预期
- [ ] panel 关闭后主界面恢复正常

## 5. Popup 场景

触发并检查以下 popup：

- [ ] HITL approval popup
- [ ] OAuth popup
- [ ] AskUser popup
- [ ] Rewind popup

每个 popup 检查：

- [ ] popup 不白屏
- [ ] popup 打开时 panel/input 焦点不乱
- [ ] Esc 能关闭 popup
- [ ] Enter 行为符合预期
- [ ] popup 关闭后主界面恢复正常

### Esc 优先级

- [ ] 有 popup 时，Esc 优先关闭 popup
- [ ] 无 popup 但有 panel 时，Esc 关闭 panel
- [ ] 无 popup / panel 时，Esc 不触发异常状态

### AskUserPopup 特别检查

- [ ] 显示的问题与 agent 实际问题一致
- [ ] 选择选项后 UI 状态正确更新
- [ ] 按 Enter 后是否真的把答案提交回 agent
- [ ] 如果只关闭 popup、不提交答案，记录为独立问题跟进

## 6. StatusBar

- [ ] 状态栏第一行显示 `provider/model_name` 格式（如 `anthropic/claude-sonnet-4-20250514`），而非短别名
- [ ] provider/model 显示为整体统一样式
- [ ] SetupWizard 页面的 provider 信息也显示完整模型名
- [ ] 模型切换时（Ctrl+T / /model 面板）状态栏实时更新

## 7. 输入历史

- [ ] 提交消息后关闭重启 Peri，之前的历史依然存在（`~/.peri/input-history.json` 持久化）
- [ ] ↑ 浏览历史，首次进入历史模式时当前输入文本被保存为草稿
- [ ] 浏览到最旧位置后 ↓ 返回编辑态，草稿自动恢复
- [ ] 纯空白文本（仅空格）不保存为草稿
- [ ] 连续输入相同文本不入栈（去重）
- [ ] 历史容量上限 1000 条

## 8. 预测输入

- [ ] Agent 回复完成后，输入区下方出现灰色预测文本
- [ ] 输入区为空 + 有预测文本时，Tab 接受预测并注入到输入框
- [ ] 打印任意字符后预测文本消失
- [ ] Enter 提交消息后预测文本消失

## 9. macOS Option 键兼容层

- [ ] macOS 终端：Alt+M 循环切换模型（等价 Ctrl+T）
- [ ] macOS 终端：Alt+Shift+M 循环切换 Provider（等价 Ctrl+Shift+T）
- [ ] 标准终端：Ctrl+T / Ctrl+Shift+T 行为不变

## 10. 极小终端 / 重绘稳定性

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

---

## 自动验证结果

提交前已通过：

```bash
cargo check -p peri-tui
cargo clippy -p peri-tui --lib
cargo test -p peri-tui --lib
lefthook run pre-commit
```

- `peri-tui --lib` 全量：**316 passed**
- `cargo check`：通过
- `cargo clippy`：通过（warnings 均为既有代码）
