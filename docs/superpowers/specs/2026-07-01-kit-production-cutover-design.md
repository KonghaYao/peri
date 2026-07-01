# peri-tui 切换至 ratatui-kit 生产级 — 设计与迭代计划

**日期**：2026-07-01
**作者**：项目主管（cron 驱动多轮迭代）
**状态**：设计中 — 2026-07-01 23:50 第 1 轮

## 目标（用户原始指令）

把 peri-tui 改为 `/ratatui-kit` 的模式，整个界面达到生产级别能用。直接删除历史的旧逻辑。完全完成输入框、消息流渲染的正确性，history 等面板的所有可用性功能及 rewind、输入缓冲区等高级功能也应该一应俱全。

## 现状（探查后实测）

### 入口分叉（`peri-tui/src/main.rs::run_tui`）

```rust
#[cfg(not(feature = "use-kit"))]  → run_app() → runtime::main_loop::run  [生产路径]
#[cfg(feature = "use-kit")]       → kit::entry::run_kit_fullscreen()     [facade]
```

`use-kit` 默认 OFF。`run_kit_fullscreen()` 仅做 `element!(AppShell).fullscreen()`——**没有 App、没有 ACP server、没有 ACP client、没有 panic hook**。InputArea 写 `SUBMIT_PENDING/SUBMIT_TEXT` atoms，**没有任何消费者**。

### kit/ 组件就绪度：30%

| 区块 | 状态 |
|------|------|
| entry / app_shell / atoms / acp_bridge / acp_events / status_bar / input_area | ✅ 接入渲染树（minimal closed loop） |
| `layout.rs::SessionColumn` | ⚠️ 用占位文本 `Messages (N committed, M current)` 替代 MessageArea |
| `message_area.rs` | ⚠️ `#[component]` 已实现（调 `view_render::render_v2_vm`），**未接入 SessionColumn** |
| `setup_wizard.rs` | ⚠️ "Step 1/5: TODO" 纯桩 |
| `event_handlers.rs` | ⚠️ Ctrl+O / Ctrl+P 是空桩 |
| 14 个 panels/* | ❌ mock 数据，未接入；无面板打开/切换机制 |
| 4 个 popups/* | ❌ mock 数据，未接入 |
| `mention_popup` / `slash_completion` | ⚠️ 已实现，未挂父组件 |
| atoms 覆盖 | ❌ 缺：当前活跃 PanelKind、Panel 数据、ThreadStore、CronScheduler、ACP client 句柄 |

### kit → legacy 依赖（最少 4 处）

```
kit/acp_bridge.rs:8-9   → state_machine::current_turn::CurrentTurn
kit/acp_bridge.rs:9     → state_machine::event::AcpEventData
kit/acp_events.rs:7-8   → 同上
kit/message_area.rs:16  → render::view_render::render_v2_vm
```

`ui::theme` 被 16+ 文件依赖——但 theme 是颜色常量，可保留或迁出。

### 5 大用户功能现状（kit 路径）

| 功能 | 状态 |
|------|------|
| 输入框 | ⚠️ 自实现 EditorState（非 tui_textarea），无 history nav、无 @mention、无 slash 补全、提交无 ACP 消费 |
| 消息流 | ❌ SessionColumn 显示占位文本，未接 MessageArea |
| History 面板 | ❌ ThreadBrowserPanel mock 数据，未接 ThreadStore |
| Rewind | ❌ 双击 Esc、/rewind N、RewindPopup 全无 kit 实现 |
| 输入缓冲区 | ❌ kit InputArea 的 EditorState 切会话即丢 |
| 鼠标滚动 / 复制 / 粘贴 / 选中 | ⚠️ Paste InputArea 已处理；其他全无 |

## 设计：分阶段迁移（非 Big-Bang）

### 决策：保留 legacy 作为生产路径，逐步把 kit 拉到功能对等 → 切默认 → 删 legacy

**理由**：
- kit 当前是 facade，big-bang 切换会立刻让 TUI 完全不可用
- 用户已睡，无法验证；不可逆动作（删除 legacy）必须等 kit 真的能跑
- cron 30 分钟一轮，自然适合分阶段
- legacy 已经稳定（1120 测试通过），承担生产风险最低

### 阶段划分（每阶段 = 1~2 个 cron 窗口）

| # | 阶段 | 验证 | 风险 |
|---|------|------|------|
| **S1** | SessionColumn 接 MessageArea（占位文本→真实渲染） | `cargo run --features use-kit` 能看到消息流（mock 数据） | 低 |
| **S2** | `run_kit_fullscreen` 复用 run_app 的 App+ACP 启动逻辑（提取公共函数） | use-kit 启动后有真实 ACP server、client、event channel | 中 |
| **S3** | acp_bridge 接入真实 ACP 事件流（fan-out from main_loop or 独立通道） | 流式消息实时显示 | 中 |
| **S4** | SUBMIT_TEXT 消费者：spawn 后台 task 把 atom → `acp_client.prompt()` | 输入框 Enter 能发消息给 Agent | 中 |
| **S5** | Atoms 扩展：PanelKind / PopupKind / ThreadStore / Cron / Model / Provider 等 | 面板机制就绪 | 低 |
| **S6** | Panel registry + Ctrl+T/P/O 快捷键 + Esc 关闭 + open/close 机制 | 14 panels 接入数据 + 可切换 | 高 |
| **S7** | 4 Popups（HITL/AskUser/Rewind/OAuth）接入 InteractionPrompt | popup 功能可用 | 高 |
| **S8** | InputArea 升级：history nav、@mention、slash 补全、输入缓冲区持久化、鼠标 | 输入框功能对齐 legacy | 中 |
| **S9** | Status bar 扩展：model/provider/CPU/MEM/context + hints 行 | 状态栏对齐 | 低 |
| **S10** | Rewind 路径：双击 Esc 触发 + RewindPopup + label 路由 | /rewind N 生效 | 中 |
| **S11** | acp_bridge/events 解耦 state_machine（CurrentTurn/AcpEventData 移到共享层） | kit 零 legacy state_machine 依赖 | 中 |
| **S12** | 切默认 feature `default = ["use-kit"]` | 默认启动走 kit | 高 |
| **S13** | 删除 legacy：runtime/main_loop.rs、apply_context.rs、effect.rs、state_machine/、ui/main_ui/、app/agent_ops/* 等 | 净减 ~30k 行 | 高 |

### 阶段顺序的拓扑约束

- S1 独立，可立即做
- S2 → S3 → S4 串行（无 ACP 就无 submit）
- S5 → S6 → S7（panel/popup 机制）
- S8 / S9 / S10 可与 S6-S7 并行
- S11 必须在 S13 之前
- S12 必须在所有 S1-S11 完成后
- S13 必须在 S12 之后

### 高优先级 "用户可见 bug"（来自审计）

1. **kit 路径下打字按 Enter 没反应**（SUBMIT 无消费者） — S4 解决
2. **kit 路径下完全无消息显示**（SessionColumn 占位文本） — S1 解决
3. **kit 路径下无面板可打开** — S6 解决
4. **kit 路径下无 popup**（HITL/AskUser 等阻塞 Agent 也无 UI） — S7 解决
5. **kit 路径下 Esc 双击无 rewind** — S10 解决

## 本轮（Iteration 1）落地的具体改动

**Scope**：仅做 S1（最低风险、最高可见 ROI）。

### 改动 1：SessionColumn 接入 MessageArea

`peri-tui/src/kit/layout.rs`：
- 删除占位 `Text(text: Paragraph::new(... "Messages (N committed, ...)"))`
- 改为 `MessageArea(view_models: .., current_turn: .., scroll_offset: .., loading: .., width: ..)`
- Props 从 atoms 读取（已订阅 VIEW_MODELS / SCROLL_OFFSET / ACP_STATE）

### 改动 2： kit/layout.rs 的 width 探测

MessageArea 需要终端宽度做 markdown 折行——ratatui-kit 组件目前没有直接获取 area 的 API（render 时才有）。
- 方案：先用固定宽度 100（占位），后续 S6 阶段通过 use_layout_query 或 hook 完善
- 验证：cargo check + cargo run --features use-kit 启动后看到消息流（mock）

### 不在本轮 scope

- S2-S13：留待后续 cron 窗口
- 用户功能（输入框 history / Rewind / clipboard / mouse）：S4/S8/S10 阶段处理

## 验证

```bash
cargo check -p peri-tui --features use-kit
cargo check -p peri-tui
cargo test -p peri-tui --lib
```

不验证：手动启动 TUI 打字（用户睡着，无法人工验证；kit 路径目前不消费 SUBMIT 也没法验证 end-to-end）。

## 后续 cron 窗口的工作记忆

每轮 cron 启动时，先读 `memory/v2-migration-status.md` + `memory/kit-cutover-progress.md`（本轮新建）确认上一轮进度，再继续。

## 风险

1. **用户醒来发现没全完成** — 设定预期：本轮只做 S1，完整迁移需 10+ cron 窗口
2. **cargo build 失败** — 每步 cargo check 验证；若失败回滚
3. **不推远程** — 严格遵守 `never-push-remote.md`
4. **不要在 facade 阶段就切换默认 feature** — S12 之前 `use-kit` 仍非默认
