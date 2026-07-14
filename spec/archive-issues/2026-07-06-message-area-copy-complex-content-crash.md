> 归档于 2026-07-10，原路径 spec/issues/2026-07-06-message-area-copy-complex-content-crash.md

# Message Area 复制操作导致 TUI 崩溃/卡死

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-06
**修复日期**：2026-07-06

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
| 2026-07-06 | Open | In Review | agent | 推测 PTY buffer 饱和，实施 ScrollUp/ScrollDown wake 节流（32ms） |
| 2026-07-06 | In Review | In Review | agent | 节流未生效。用户反馈"单滚动不卡死，复制后才卡死"——彻底推翻 PTY 假设 |
| 2026-07-06 | In Review | In Review | agent | **真正根因**：status_bar.rs:141 在 render body 中写 COPY_MESSAGE_UNTIL atom，违反 ratatui-kit 状态管理铁律（同 issue_2026-07-03-tui-double-slash-cpu-spike）。复制后 2s 过渡时 status_bar 检测到 now >= until 触发 atom write → wake → render → atom write 自激回路。修复：渲染层只读判断 now < until，移除 atom 写入。同时把 arboard 调用包到 std::thread::spawn 避免阻塞 tokio worker |
| 2026-07-06 | In Review | Fixed | user | 用户验证：复制后连续滚动不再卡死，复制无延迟，提示正常消失。诊断埋点和基于错误假设的节流代码全部回滚 |

## 修复记录

### 修复 v4（2026-07-06）：status_bar render body 写 atom 自激回路（真正根因）

**真正根因**（再次推翻 v3 的 PTY 假设）：

用户反馈："单滚动不卡死，复制后才卡死"。日志显示从启动到复制前用户滚动 1348 次都没问题，复制后约 2 秒的窗口内滚动也正常（处理了 107 个 ScrollUp 事件），**正好在 2 秒过期点**卡死。

`status_bar.rs:141` 在 render body 中写入 atom：

```rust
if let Some(until) = *copy_until.read() {
    if now < until {
        // 显示"已复制 N 字符"
    } else {
        *copy_until.write() = None;   // ← render body 中写 atom！违反铁律
    }
}
```

时间线：
```
T+0.000s  Up(Left) → mark_copy_message 写 COPY_MESSAGE_UNTIL = T+2.000s
T+0.535s  用户开始滚动,ScrollUp 持续到来
T+0.535s ~ T+2.008s  正常处理 107 个 ScrollUp(节流后 21 次渲染),
                      status_bar 始终走 now < until 分支(显示复制提示)
T+2.008s  最后一个 ScrollUp event_hits=1492,日志中断
T+2.008s ~ T+?  status_bar 第一次检测到 now >= until → 进入 else 分支
                  → 写 atom = None → 触发 wake → render → 与组件生命周期交互
                  → 形成 render → state write → render 自激回路 → TUI 卡死
```

这和 issue_2026-07-03-tui-double-slash-cpu-spike 是**完全相同的模式**：SlashCompletion 在 render body 写 SLASH_SELECTED_INDEX atom，在 slash_active 从 true→false 过渡时引发自激回路。

CLAUDE.md 已明确写：
> **[TRAP]** ratatui-kit render body 中禁止写 atom——render 期间任何 atom 写入会与组件生命周期交互形成 render → state write → render 自激回路。

**为什么前 8 次失败**：所有尝试都在治理"事件/状态写入频率"和"渲染管线"，**没有触及 status_bar 这个违反铁律的写入点**。日志显示复制前 1348 次滚动正常,本来就该排除"高频滚动"假设。

### 实施 v4

#### 1. status_bar.rs：移除 render body 中的 atom 写入

```rust
// 修复前：
if let Some(until) = *copy_until.read() {
    if now < until { /* 显示提示 */ }
    else { *copy_until.write() = None; }   // ← 自激源
}

// 修复后：只读判断
let copy_active = copy_until.read().map_or(false, |until| now < until);
if copy_active { /* 显示提示 */ }
```

atom 保留旧 `Some(until)`,但渲染层用 `now < until` 做只读判断。下次 `mark_copy_message` 用新 `Instant` 覆盖 atom,无副作用。

#### 2. message_area.rs：arboard 调用包到 std::thread::spawn

```rust
fn copy_selected_text_to_clipboard(text: String) {
    std::thread::spawn(move || {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(text);
        }
    });
}
```

原注释"在 tokio 主任务中运行"是**错误前提**——tokio multi-thread runtime 的 worker **不是**主线程,主线程在 `block_on` 等待。CLAUDE.md 明确要求剪贴板等阻塞 I/O 用 `std::thread::spawn`。这一项解释了 Up(Left) 后的 535ms 静默期(arboard 同步调用阻塞 tokio worker)。

### 待用户验证

1. 复制后立即连续滚动——是否仍卡死?(应不再卡死)
2. 复制时是否有可感知的延迟?(应消失,arboard 不再阻塞)
3. "已复制 N 字符"提示是否在 2 秒后正常消失?(应正常)

---

### 修复 v3（2026-07-06，已被 v4 推翻）：滚轮 wake 节流

**真正根因**（推翻 select! race 假设）：

`ratatui-kit` `render_loop` 在 `tokio` worker 上单线程运行，`self.render(terminal)` 调用 `terminal.draw` 同步写 stdout。macOS 鼠标滚轮高频事件（≈100-125Hz）让 crossterm `VecDeque` 无限积压，ratatui-kit 每个 ScrollUp 触发一次完整渲染循环：

```
ScrollUp 每 8ms 到达（crossterm 内部缓冲无上限）
  → ratatui-kit select 命中 Right(next_event) → dispatch
  → scroll_state.write() 触发 wake
  → 回到 select 顶 → render() → terminal.draw() → stdout.write_all()
  → PTY buffer (macOS ~64KB) 渐满
  → stdout.write_all 阻塞（系统调用同步阻塞 tokio worker）
  → 渲染永远赶不上事件产生速度 → PTY 永久饱和
  → worker 在 stdout write 中阻塞，tracing 也无法在同线程执行
  → 日志突然中断，TUI 无响应，需要外部 kill
```

**关键日志证据**（`.tmp/agent-tui.log`，2026-07-06T12:30:27-28）：
- `next_event` 命中 216 次，`wait Ready` 命中 189 次（同步交替）
- 事件间隔稳定 8-17ms
- 日志在 `event_hits=216 wait_hits=189` 处突然中断，**无 panic、无 shutdown**
- 说明 worker 被同步系统调用阻塞，进程仍活但无响应

**为什么 7 次失败**：上一位程序员的所有 7 次尝试都在治理"事件/状态写入频率"，没有触及渲染管线的 stdout 阻塞。

### 实施（message_area.rs）

**节流策略**：ScrollUp/ScrollDown 持续累积到 `scroll_state`（用 `write_no_update` 不触发 wake），每帧 wake 限频到 ≥ 32ms 间隔：

```rust
const SCROLL_WAKE_THROTTLE: Duration = Duration::from_millis(32);

// use_state 增加：
let scroll_wake_at = hooks.use_state(|| None::<Instant>);

// mouse handler 闭包内 ScrollUp/ScrollDown：
let now = Instant::now();
let should_wake = scroll_wake_at_handler
    .read()
    .as_ref()
    .map_or(true, |t| now.duration_since(*t) >= SCROLL_WAKE_THROTTLE);
scroll_state_handler.write_no_update().scroll_up();  // 累积但不 wake
if should_wake {
    *scroll_wake_at_handler.write() = Some(now);     // 触发渲染
}
```

### 已知遗留

1. **用户停止滚动后画面可能落后 1-2 行**：最后一次节流跳过的 wake 不会被消费，但下次任何交互（mouse Moved、键盘）触发 message_area re-render 时会自动修正。
2. **第一次复制场景仍可能有少量积压**：Drag 事件间隔（10-30ms）已比 ScrollUp 稀疏，未节流。如果用户复制后立即拖动复杂数千行内容，理论仍可能 PTY 饱和。本次未处理 Drag 节流以避免影响选中精度。

### 待用户验证

1. 第二次复制后连续滚动——是否仍卡死？
2. 滚动体感是否流畅（32ms = ~31Hz 渲染）？如果不够流畅可调到 24ms（~41Hz）；如果仍卡死可调到 48ms（~20Hz）。
3. 验证通过后回滚诊断埋点（fork `ratatui-kit` `tree.rs` + `Cargo.toml`，以及 message_area.rs 中 4 处 `tracing::info!`）。

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

---

## 诊断埋点 v1（2026-07-06，第二轮调查）

**新发现**：ratatui-kit 0.7.2 fork（`KonghaYao/ratatui-kit@45b9b3a`）的 `render_loop` 使用 `futures::future::select`（非公平）。`select` 先 poll A（`wait()`），A 总 Ready 时 B（`next_event()`）永远不被 poll——crossterm 内部 buffer 鼠标事件积压但读不到。7 次失败修复都未触及根因，问题在架构层 race。

**已埋点（临时，待复现日志回归根因后回滚）**：

1. `~/.cargo/git/checkouts/ratatui-kit-57880b1120009d67/45b9b3a/crates/ratatui-kit/Cargo.toml`：加 `tracing = "0.1"` dep
2. `~/.cargo/git/checkouts/ratatui-kit-57880b1120009d67/45b9b3a/crates/ratatui-kit/src/render/tree.rs`：在 `select!` 两个分支加 `tracing::info!`，记录 `wait_hits` / `event_hits` 计数器
3. `peri-tui/src/kit/message_area.rs`：
   - mouse 事件入口（line 419 附近）记录 `msg-area: mouse event`
   - line_cache.key 变化（line 285 附近）记录 `msg-area: line_cache key changed` + `auto_scroll.set(true)`
   - spinner advance（line 670 附近）记录 `msg-area: spinner advance`

**复现步骤**：

```bash
# 启动（保证日志写到 .tmp/agent-tui.log）
RUST_LOG_FILE=.tmp/agent-tui.log cargo run -p peri-tui

# 复现：完成第一次复制 → 第二次拖拽中途卡死 → Ctrl+C kill 进程

# 提供日志（末尾 500-1000 行最关键）
tail -1000 .tmp/agent-tui.log
```

**预期诊断结论**：

- 若日志显示 `select: wait Ready` 计数器每秒数百次增长，`select: next_event` 在 Drag 后停滞 → 确认 select! race，需修 fork 改 `tokio::select!`
- 若 `msg-area: spinner advance` 在卡死期间持续刷屏 → 确认 spinner 自激循环未真正修复（c7207c37 仅加了壁钟节流，但 delta 总是 > 0 仍触发 wake）
- 若 `msg-area: line_cache key changed` 频繁出现 → 某个 atom 在每次 render 后变化（VIEW_MODELS / TODO_ITEMS / ACP_STATE）

**回滚**：直接 `git checkout` `~/.cargo/git/checkouts/ratatui-kit-57880b1120009d67/45b9b3a` 中的两个文件，并撤销 `peri-tui/src/kit/message_area.rs` 中的 4 处 `tracing::info!`。
