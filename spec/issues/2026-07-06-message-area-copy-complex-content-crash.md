# Message Area 复制操作导致 TUI 崩溃/卡死

**状态**：Open
**优先级**：高
**创建日期**：2026-07-06

## 问题描述

在 TUI 的 Message Area 中使用鼠标拖拽选择文本进行复制时，TUI 会出现崩溃或完全卡死的异常行为。首次复制操作有时可以正常完成，但**第二次复制必然触发异常**——早期表现为鼠标抬起时崩溃，修复后表现为拖拽中途 TUI 完全无响应（卡死），只能通过 kill 终止进程。

## 症状详情

### 现象 1（原始崩溃，已部分缓解）

- 触发动作：鼠标按下并拖拽选择消息文本，鼠标抬起时触发复制流程。
- 触发内容：复杂内容，例如代码块、工具输出、Markdown 表格等复杂渲染内容。
- 实际表现：鼠标抬起时程序直接崩溃。

### 现象 2（第二次复制卡死，2026-07-06 更新）

- **复现规律**：**第二次复制必然卡死**。第一次 Mouse Down → Drag → Up 可以正常完成选区+复制，第二次复制在 Drag 中途 TUI 完全无响应。
- **卡死时机**：拖拽过程中（Drag 事件阶段），并非等待 Up 事件。日志中最后一条记录为 `Drag(Left)` 后日志中断，无 panic 堆栈、无错误日志、无正常 shutdown 序列——进程直接卡死。
- **TUI 状态**：完全无响应，无法通过任何键盘/鼠标操作恢复，只能 `kill` 终止进程。

**卡死点日志证据**（完整日志见 `/Users/konghayao/code/ai/perihelion/.tmp/agent-tui.log`）：

```text
# 第一次复制：正常完成
2026-07-06T08:52:07.369  MouseEvent { kind: Drag(Left), column: 18, row: 11 }
2026-07-06T08:52:08.669  MouseEvent { kind: Drag(Left), column: 18, row: 3 }
2026-07-06T08:52:08.835  MouseEvent { kind: Up(Left), column: 18, row: 3 }    # ← Up 正常到达

# 中间有短暂滚动操作...

# 第二次复制：Drag 中途日志中断
2026-07-06T08:52:10.754  MouseEvent { kind: Down(Left), column: 19, row: 8 }
2026-07-06T08:52:10.821  MouseEvent { kind: Drag(Left), column: 19, row: 8 }
2026-07-06T08:52:10.852  MouseEvent { kind: Drag(Left), column: 20, row: 8 }  # ← 日志在此中断，Up 从未出现
```

日志中在卡死期间持续输出 Message Area 渲染诊断信息，说明渲染循环仍在运行，但事件处理已卡死。

## 复现条件

- **复现频率**：第二次复制必现
- **触发步骤**：
  1. 打开 TUI，在 Message Area 中完成一次鼠标拖拽选中+复制（正常工作）。
  2. 再次在 Message Area 中鼠标按下拖拽尝试选中文本。
  3. 拖拽过程中 TUI 卡死，无响应。
- **环境**：macOS；当前项目 TUI（ratatui-kit MessageArea）。

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— Message Area 鼠标 Down/Drag/Up 事件处理与复制触发逻辑所在文件。
- `peri-tui/src/kit/text_selection.rs` —— 消息区文本选区、高亮与文本提取逻辑所在文件。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-06 | — | Open | agent | 创建（初始崩溃现象） |
| 2026-07-06 | Open | Open | agent | 追加现象 2：第二次复制必然卡死，Drag 中途 TUI 无响应，日志无 panic |
| 2026-07-06 | Open | Open | agent | 追加探索记录：7 项尝试均未修复，记录 3 个剩余猜想及建议下一步 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）

---

## 探索记录（2026-07-06 agent）

### 现象 3（复制后滑动卡死）

用户报告：复制完成之后，滑动几下就卡死了。日志末端正常（`msg-area diag` 输出到 `11:24:41.600`，随后 18 秒无输出），无 panic、无错误日志。

### 已完成的尝试

| # | 假设 | 尝试 | 结果 |
|---|------|------|------|
| 1 | 复制高亮坐标计算错误导致 panic | 添加 `visual_to_line_position` 二分查找转换视觉→物理坐标 | 坐标正确了，但第二次复制 Drag 仍然卡死 |
| 2 | `selected_text` 未清理导致每帧高亮全量行 | `sel.clear()` 复制后立即清除选区 | 仍卡死 |
| 3 | Drag 事件高频写入 `scroll_state` 导致渲染积压 | Drag 不写 scroll_state，仅写 text_sel + auto_scroll | 仍卡死 |
| 4 | 窗口 resize 触发高频 `build_wrap_map` 重建 → crossterm buffer 溢出 → `next_event()` 死锁 | 添加 100ms resize debounce | 仍卡死 |
| 5 | 每帧诊断 `tracing::info!` 大量文件 I/O 拖慢渲染 | 移除所有 `msg-area diag`、`msg-sel: *` 诊断日志 | 仍卡死 |
| 6 | `auto_scroll.set(false)` 每次鼠标事件无条件触发 atom write + wake | 改为 `if auto_scroll.get() { auto_scroll.set(false); }` | 仍卡死 |
| 7 | 终端 focus change 导致 `next_event()` 永久阻塞 | 心跳从 5s → 500ms；AppShell 订阅 `RENDER_HEARTBEAT` atom | 仍卡死 |

### 剩余猜想

#### 猜想 A：ratatui-kit 事件循环 `select!` 核心问题

ratatui-kit 的 `Tree::render_loop` 使用 `futures::select!{wait(), next_event()}`。当 `next_event()` 阻塞（如 macOS 终端 focus change 不发送 FocusGained），即使组件 `wait()` 被心跳唤醒一次，render 后 `wait()` 又 Pending，`select!` 再次卡在 `next_event()` 上。

**心跳无法根治**：心跳只能以固定间隔唤醒 `wait()`，但无法替代 `next_event()` 返回真正终端事件。当 `next_event()` 永久阻塞时，render loop 只能在心跳间隔之间短暂"苏醒"一帧，然后再次阻塞。

**可能的修复方向**：
- 给 `terminal.next_event()` 加超时包装（但 ratatui-kit 的 `Terminal` 类型封装了 crossterm `EventStream`，外部无法注入超时）
- 用自定义事件源替代 crossterm 的 `EventStream`（改动 ratatui-kit 内部）

#### 猜想 B：`use_atom` 订阅在某些状态下失效

心跳 atom `RENDER_HEARTBEAT` 通过 `use_atom` 在 AppShell 订阅。如果 AppShell 的 `ElementKey` 在 render 过程中变化（比如组件树重建导致 key 重新分配），旧 key 的 waker 可能无法被清除，而新 key 的 waker 可能因某些条件未注册。

日志中 18 秒无渲染输出，暗示心跳要么没触发、要么触发了但 AppShell 没收到 wake。需验证：
1. tokio 心跳 task 是否退出（检查 shutdown token 是否被误 cancel）
2. `WakerMap` 中 AppShell 的 key 是否存在
3. ratatui-kit v0.45 的 `poll_change` 是否在 `try_write` 失败时正确返回 Pending（而不是跳过 waker 注册）

#### 猜想 C：死锁——atom 读写嵌套

心跳写 `RENDER_HEARTBEAT` → `WakerMap::wake()` → 唤醒 AppShell → render → MessageArea render → 读 `text_sel` use_state。如果在某些路径下 `text_sel.write()` 持有锁时心跳触发了 `RENDER_HEARTBEAT.set()`，可能导致 `try_write` 失败但不注册 waker 的竞态。

### 当前代码状态

- `message_area.rs`：移除所有每帧诊断日志；`auto_scroll.set(false)` 条件写入；100ms resize debounce
- `entry.rs`：心跳间隔 500ms
- `app_shell.rs`：订阅 `RENDER_HEARTBEAT` atom

### 建议下一步

1. **在 ratatui-kit render loop 中加诊断**：`tracing` 每个 `select!` 分支的进入/退出，确认识别阻塞点在 `wait()` 还是 `next_event()`
2. **验证心跳是否生效**：在 `RENDER_HEARTBEAT.set()` 处加 `tracing::info!`，确认心跳 task 存活且值在递增
3. **验证 AppShell 是否收到 heartbeat wake**：在 `AppShell` 组件入口加 heartbeat counter 日志
4. **测试最小复现**：在一个纯 ScrollView + 文本选区的 minimal app 中复现，排除项目代码干扰
