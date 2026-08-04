# TUI 弹窗系统

> 本文档描述 PopupOverlay 弹窗容器的完整设计规范，包括 HITL 审批、AskUser 问答、OAuth 授权、Confirm 确认、下载进度、模型快速切换与 SetupWizard 向导。

---

## 7. PopupOverlay 弹窗容器

`PopupOverlay`（`peri-tui/src/kit/popup_overlay.rs`）是 AppShell 根级覆盖层，订阅 `POPUP_KIND` atom（`atoms.rs`），与 PanelOverlay 平级但优先级更高（Esc 链：popup → @mention/slash → panel）。`PopupKind` 共 7 种：

| PopupKind | 渲染组件 | 触发源 |
|-----------|---------|--------|
| Hitl | HitlPopup | `HitlPending` 事件（`acp_events/system.rs::handle_hitl_pending`） |
| AskUser | （不渲染） | AskUser 已迁移为 Panel（`render_empty()`） |
| Rewind | RewindPopup | 双击 Esc（`event_handlers.rs`），非事件驱动 |
| OAuth | OAuthPopup | `OauthNeeded` 事件（`handle_oauth_needed`） |
| Confirm | ConfirmPopup | 组件内 `open_popup`（AskUser 拒绝确认 / Thread 切换确认） |
| Download | DownloadProgressPopup | ThemePanel `Ctrl+D` 触发主题下载 |
| ModelQuickSwitch | ModelQuickSwitchPopup | StatusBarRow1 点击模型段（`status_bar.rs`） |

渲染语义：

- 居中弹窗（Hitl/Rewind/OAuth/Confirm/Download）：`Positioned` 定位 + `clear: true`，尺寸 = `term - 4` 与 theme `component.popup.modal_max_width/height` 取 min，水平垂直居中，避免 Modal 整屏背景绘制导致白屏。
- ModelQuickSwitch 为**小弹出层**：组件内部按 `MODEL_SWITCH_ANCHOR` 自定位（锚定在状态栏模型段上方，上方空间不足时翻转到下方），不走居中 `render_popup`。
- 无弹窗激活时返回零尺寸 `Positioned`，不消耗布局。

操作：`open_popup(kind)` 覆盖式打开（已打开的弹窗被替换）；`close_popup()` 关闭并**同步清空对应 payload atom**（I21-C：Hitl 清 HITL_PENDING/HITL_REQUEST_ID，AskUser 清 ASK_USER_PENDING/ASK_USER_REQUEST_ID，OAuth 清 OAUTH_INFO 等；Rewind 的 REWIND_PREVIEW 保留——候选跟随会话生命周期；ModelQuickSwitch 无 payload atom）。

### 7.1 HITL Permission Popup

```text
────────────────────────── Permission Request ─────────────────────────
  Tool wants to run
  Bash
  ┌──────────────────────────────────────────────────────────────────┐
  │ cargo test -p peri-tui --lib                                      │
  └──────────────────────────────────────────────────────────────────┘
  (batch: +2 more tools)

  Enter::approve · Esc::deny · mouse-left::approve
────────────────────────────────────────────────────────────────────────
```

能力：展示工具名和输入参数，支持用户审批或拒绝工具执行。数据来自 `HITL_PENDING` atom（`HitlPending` 事件写入，展示 agent 实际触发的工具调用）。

交互与渲染（`popups/hitl_popup.rs`）：

- `Enter` 批准（`HitlResponseAction::Approve`）、`Esc` 拒绝（`Reject`）、弹窗区域内鼠标左键点击 = 批准；处理器使用 High 优先级 + `hit_test: true`，先于全局 Esc 链执行。
- 工具输入渲染为 pretty JSON，截断 400 字符 / 8 行，超出显示截断提示；批次附加工具（batch）最多展示 4 个 + 剩余计数。
- 响应链路：`HitlResponseAction` → `HITL_RESPONSE_TX` → `hitl_response.rs` 消费者 task → `AcpTuiClient::send_response`。协议使用 `outcome` 内部标签：`{"outcome": {"outcome": "selected", "optionId": "allow_once"}}`（批准）/ `{"outcome": {"outcome": "cancelled"}}`（拒绝）——当前只有 allow_once 与 cancelled 两个选项，无 Allow session / Deny 三按钮。

### 7.2 AskUser 问答（已迁移为 Panel）

用户问答**已从弹窗迁移为 Panel**：`POPUP_KIND::AskUser` 分支渲染空（`render_empty()`，注释「AskUser 已迁移为 Panel」），AskUser 事件改为 `open_panel(PanelKind::AskUser)` 自动打开内联面板（`panels/ask_user.rs`，`MutexGroup::AskUser`）。完整设计规范见 tui-panels.md §6.16。

- 交互模型：Tab/Shift+Tab 切换问题、↑/↓ 选项、Space 选中/取消、Enter 下一题/提交、Esc 取消（→ Confirm 确认弹窗 → 发送 `Reject`）。
- 遗留：`popups/ask_user_popup.rs`（AskUserPopup 组件）保留但**不再被 PopupOverlay 渲染**；`ask_user_action.rs` 消费者（Submit/Cancel/Reject → `AcpTuiClient::send_response`，`ElicitationAction` 内部标签）仍为唯一响应通道。

### 7.3 Rewind Popup

Rewind 为 v2 两段式流程（双击 Esc 触发，非 `/rewind` 命令——slash 无 rewind 条目）。弹窗三态：

```text
─────────────────────────── Rewind Preview ────────────────────────────
  Messages to remove
  > 修一下这个 bug
    继续优化面板渲染

  Enter::preview · Esc::close
────────────────────────────────────────────────────────────────────────
```

```text
──────────────────────────── Rewind Budget ────────────────────────────
  This will remove the latest user turn and derived messages.

  Files touched
  - peri-tui/src/kit/panels/plugin.rs
  - peri-tui/src/kit/atoms.rs

  Enter::confirm-rewind · Esc::back
────────────────────────────────────────────────────────────────────────
```

- **Candidates 态**：双击 Esc → `rewind_candidates.rs::spawn_candidates_query` 实时查询 `session/rewind-candidates` RPC，写入 `REWIND_PREVIEW` atom；候选只含 user 消息，↑/↓ 选择。
- **Budget 态**：候选 Enter → `RewindAction::Preview`（`rewind_action.rs` 消费者）暂存目标文本到 `REWIND_TARGET_TEXT` → 查询 `session/rewind-preview` 预算 → 预算空则立即执行回退；预算非空则写 `REWIND_BUDGET_STATE = Files(预算)`，弹窗切预算视图（文件影响范围），Enter 发送 `RewindAction::Confirm` 执行（恒 `revert_files=true`）。
- **Executing 态**：等待 `RewindCompleted` 事件——回退完成后 `REWIND_BUDGET_STATE` 复位、目标文本写回 `INPUT_RESTORE_TEXT` 恢复编辑态、弹窗关闭（仅当弹窗仍在显示时，防误关其他弹窗）；`RewindError` 事件渲染系统提示（不复用 CompactError 文案）。
- 查询失败写 `REWIND_QUERY_ERROR`；Esc 从 Budget 返回候选视图。

### 7.4 OAuth Popup

```text
──────────────────────────── OAuth Required ──────────────────────────
  MCP server requires browser authorization.

  Server: langfuse
  URL:    https://...

  Ctrl+O::open-in-browser · Enter::close · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：展示 MCP OAuth 授权信息（`OAUTH_INFO` atom，由 `OauthNeeded` 事件写入），辅助用户完成外部登录流程。交互（`popups/oauth_popup.rs`）：

- `Ctrl+O`：调用系统 `open` 命令打开浏览器到 `auth_url`（best-effort，失败只记日志）。
- `Enter`：关闭 popup（ACP server 自身的 OAuth 完成回调会再推送状态事件刷新 UI；本地不缓存授权码，避免误用陈旧凭据）。
- `Esc`：取消（全局 Esc 链处理）。

### 7.5 Confirm Popup

```text
────────────────────────────── Confirm ────────────────────────────────
  Are you sure?

  This will reject the question and notify the agent.

  [Confirm]                    [Cancel]

  Enter::confirm · Esc::cancel
────────────────────────────────────────────────────────────────────────
```

能力：通用确认弹窗——从 `CONFIRM_PAYLOAD` atom 读取（title / message / details / pending_action），`Enter` 执行确认动作，`Esc` 取消关闭。当前两类调用方：

- **RejectAskUser**：AskUserPanel 按 Esc 取消时弹出（`panels/ask_user.rs`），确认后经 `ASK_USER_RESPONSE_TX` 发送 `AskUserResponseAction::Reject`，防止 agent 永久挂起。
- **ThreadSwitch**：ThreadBrowser 切换会话前确认（`thread_load_consumer.rs`），确认后继续加载目标 thread。

### 7.6 Download Progress Popup

```text
────────────────────────── Download Progress ──────────────────────────
  Downloading themes...

  ✓ peri-dark
  ● synthwave-84    3.2 MB / 10 MB
  ○ monokai-classic

  (download in progress — Esc disabled)
────────────────────────────────────────────────────────────────────────
```

能力：展示从 GitHub 下载主题文件的进度（`DOWNLOAD_PROGRESS` atom，`DownloadProgressPayload`）。逐文件显示状态：Pending → Downloading → Done/Failed；**下载中 Esc 无效**（防误关闭），下载完成后 Esc 关闭。触发入口：ThemePanel `Ctrl+D`（`panels/theme.rs`）。

### 7.7 ModelQuickSwitch Popup

```text
  ┌───────────────────────────────┐
  ▼ sonnet                        │
    opus                          │
  ❯ haiku                         │
    fable                         │
  ┌───────────────────────────────┘
```

能力：状态栏模型段（alias/model）点击弹出的**小弹出层**（非居中大 modal）。`StatusBarRow1` 点击时写 `MODEL_SWITCH_ANCHOR`（模型段起点屏幕坐标），组件 `Positioned` 到锚点上方（空间不足翻转到下方）。交互（`popups/model_quick_switch.rs`）：

- `↑/↓` 选择、`Enter` 切换并关闭（键盘全程可用）；鼠标 hover 行高亮（选中跟随悬停）、鼠标点击行直接切换并关闭、点击弹窗矩形之外关闭（dismiss-on-outside-click）；`Esc` 关闭（组件消费，全局链兜底）。
- 数据即读自 `PERI_CONFIG_HANDLE`，无独立 payload atom；行布局契约（hover/点击反推行号依赖）定义在组件内。

---

## 8. SetupWizard 向导

SetupWizard 是**根级全屏向导**（非弹窗）：`WIZARD_ACTIVE = true` 时 AppShell 渲染 SetupWizard 覆盖一切（`app_shell.rs`，最高优先级），主布局 + PopupOverlay 不渲染。

四步向导：**Language → Choose → Form → Done**。状态存储在 `SETUP_WIZARD` atom 中，显隐由 `WIZARD_ACTIVE` 控制。

触发条件：

- **首次启动自动触发**：`entry.rs` 检测 `needs_setup(&config)`（未配置 Provider）→ `WIZARD_ACTIVE = true`；向导支持 Esc/q 退出（避免首次启动锁死）。
- `/setup` 命令手动打开（`SubmitRequest::SessionControl(ToggleSetup)`）。

能力：引导用户完成语言选择、Provider 选择、表单配置（API key 等）与完成页；表单样式与 Login 面板 Edit 模式统一。完成或退出时 `WIZARD_ACTIVE = false` 恢复主界面。

---

> [返回总索引](tui-index.md)
