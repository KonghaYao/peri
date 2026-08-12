> 归档于 2026-08-11，原路径 spec/issues/2026-08-05-bg-shell-task-id-collision.md

# bg shell task_id 碰撞导致 TUI 任务条目残留

- 日期：2026-08-05
- 状态：Fixed（已验证）
- 模块：`peri-middlewares`（terminal.rs / background.rs）
- 触发条件：agent 在同一毫秒内连续发起多次 `run_in_background: true` 的 Bash 调用

## 症状

用户 TUI 中并发运行 3 个 bg shell（`sleep 8`）：进程全部正常退出、inbox 完成通知
（`[后台任务 shell-XX 已完成]`）全部到达，但 BgTaskArea 残留 2 个 `◎ shell  sleep 8`
条目永不消失（`is_active=true` 条目无自动过期）。tasks 面板（registry）为空——
3 个任务确实已完成。

## 根因

bg shell 的 task_id 生成截断了 UUID v7：

```rust
// 旧实现
format!("shell-{}", uuid::Uuid::now_v7().to_string().chars().take(8).collect::<String>())
```

UUID v7 前 48 位是毫秒时间戳 → 前 8 字符 = 时间戳高 32 位，**同一毫秒内多次调用
必然生成相同前缀**。agent 连续 3 次 Bash 工具调用落在同一毫秒（工具执行 <1ms）时：

1. 3 次 `register_with_kind` 用相同 task_id → `HashMap::insert` 覆盖注册
   （Started 事件 ×3 → TUI 显示 3 个条目；cancel 句柄只保留最后一个）
2. 完成时：第 1 个 `complete()` 命中任务 → 更新状态 + `retain` 清理所有非
   Running 任务 → 推 Completed 事件（TUI 清除 1 个条目）
3. 第 2/3 个 `complete()` 时任务已不在 registry → **`existed=false` 静默跳过**
   （"幽灵完成防护"逻辑）→ Completed 事件丢失 → TUI 残留 2 个条目

对比：bg agent 用 `bg-{完整 UUID}`（execute_bg.rs / spawner.rs），122 位熵无碰撞，
故该 bug 仅影响 bg shell。

## 修复

1. `terminal.rs`：提取 `bg_shell_task_id()`，使用完整 UUID v7（`shell-{完整 UUID}`），
   两处生成点（run_in_background 路径、同步超时 promote 路径）统一调用。
   保留 `shell-` 前缀，测试/e2e 断言（`contains("shell-")`）向后兼容。
2. `background.rs`：`complete()` 的 `existed=false` 分支由静默跳过改为 `warn!`
   日志——本次 bug 正是被该静默路径掩盖，日志化后同类问题可直接定位
   （collision / double-complete）。

## 验证

- 新增单测 `test_bg_shell_task_id_uniqueness`：连续生成 64 个 id 断言全部唯一
  （大概率落在同一毫秒，旧实现必挂）+ 前缀断言。
- 实机复现（tmux + `target/debug/peri`）：3 个并发 `sleep 8` bg shell，
  修复前残留 2 个条目（`7m46s` 证据）；修复后 3 个条目全部清除，BgTaskArea 0 残留。
- `cargo test -p peri-middlewares --lib`（terminal + background 用例）全绿；
  `cargo clippy -p peri-middlewares --all-targets` 无警告。
