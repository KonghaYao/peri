# TUI 弹窗系统

> 本文档描述 PopupOverlay 弹窗容器的完整设计规范，包括 HITL 审批、AskUser 问答、OAuth 授权、SetupWizard 向导。

---

## 7. PopupOverlay 弹窗页面设计

### 7.1 HITL Permission Popup

```text
┌────────────────────────── Permission Request ──────────────────────────┐
│ Tool wants to run                                                       │
│                                                                        │
│  Bash                                                                   │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ cargo test -p peri-tui --lib                                      │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                        │
│  [Allow once]   [Allow session]   [Deny]                               │
│                                                                        │
│  Enter::confirm · ←/→::choose · Esc::deny                         │
└────────────────────────────────────────────────────────────────────────┘
```

能力：展示工具名和输入参数，支持用户审批或拒绝工具执行。

### 7.2 AskUser Panel

用户问答面板——当 agent 调用 AskUserQuestion 工具时，自动作为 Panel 内联在 MessageArea 和 InputArea 之间渲染（与 Thread Browser 等面板一致）。Tab 键在问题间切换，当前问题显示选项列表，Space 选中/取消，Enter 跳到下一个未确认问题或全部答完后提交，Esc 取消并标记失败。

```text
──────────────────────────── Ask User ────────────────────────────
  [布局方案]  启用能力  备注
──────────────────────────────────────────────────────────────────
  请选择布局方案

  ○ 单列聊天优先
    适合窄屏和默认工作流

  ● 抽屉面板
    面板插入消息流底部，输入区隐藏

  ○ 双栏监控
    适合长期运行任务

  Tab::next-question · ↑/↓::navigate · Space::select · Enter::next · Esc::cancel
──────────────────────────────────────────────────────────────────
```

多问题已全部确认时：

```text
──────────────────────────── Ask User ────────────────────────────
  布局方案 ✓  启用能力 ✓  备注 ✓
──────────────────────────────────────────────────────────────────
  备注

  ○ 选项 A
  ● 选项 B

  Tab::next-question · ↑/↓::navigate · Space::select · Enter::submit · Esc::cancel
──────────────────────────────────────────────────────────────────
```

能力：展示 Agent 发起的结构化问题，支持 1-4 个问题批量接收；顶部用 tabs 展示所有问题，已回答项旁显示 ✓；当前 tab 展示一个问题内容，通过 `Tab` / `Shift+Tab` 切换。每个问题可为单选（○/●）或多选（☐/☑）；面板打开时隐藏 InputArea，与其他 Panel 行为一致。

### 7.3 Rewind Popup

```text
┌──────────────────────────── Rewind Preview ──────────────────────────┐
│  This will remove the latest user turn and derived messages.           │
│                                                                        │
│  Messages to remove                                                    │
│  - user: 修一下这个 bug                                                │
│  - assistant: 我会先复现...                                            │
│  - tool: Bash                                                          │
│                                                                        │
│  Files touched                                                         │
│  - peri-tui/src/kit/input_area.rs                                      │
│                                                                        │
│  [Confirm rewind]                         [Cancel]                    │
└────────────────────────────────────────────────────────────────────────┘
```

能力：在执行 `/rewind` 前展示将被回退的消息和文件影响范围。

### 7.4 OAuth Popup

```text
┌──────────────────────────── OAuth Required ──────────────────────────┐
│  MCP server requires browser authorization.                           │
│                                                                        │
│  Server: langfuse                                                      │
│  URL:    https://...                                                   │
│                                                                        │
│  1. Open URL in browser                                                │
│  2. Complete authorization                                             │
│  3. Return to Peri                                                     │
│                                                                        │
│  Enter::open-or-copy-url · Esc::close                          │
└────────────────────────────────────────────────────────────────────────┘
```

能力：展示 MCP OAuth 授权信息，辅助用户完成外部登录流程。


---

> [返回总索引](tui-index.md)
