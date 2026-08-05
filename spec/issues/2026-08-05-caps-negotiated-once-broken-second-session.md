# caps 协商值只消费一次：stdio 第 2+ 个 session 的事件门控全部错乱

**状态**：Open
**优先级**：高
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S1.1

## 问题描述

`initialize` 暂存的 `pending_caps` 在第一个 `session/new` 时被 `take()` 一次性取走，之后同一 server 进程内的第 2+ 个 session 拿不到协商值：有的注册**全 false** caps（`consume_pending_caps` 取到 None 走 `unwrap_or_default`），有的回退 **all_enabled**（`ensure_session_caps` 未命中 registry）。同一客户端的不同 session 事件门控行为不同，违反"每条 session/new、load、resume 或 fork 路径注册 session caps"的稳定不变量。

## 症状详情

- 第 1 个 `session/new`：正常（取到协商值）
- 第 2 个 `session/new`：`consume_pending_caps` 取到 `None` → 注册全 false caps
- `session/load`/`resume`/`fork` 新 session id：`ensure_session_caps` registry 未命中、pending 已空 → 注册全 true（all_enabled）caps
- 表现：`token_stats`/`replay`/`agent_event`/`agent_event_done`/`unstable_event` 有的 false 有的 true，IDE 侧 replay meta、usage meta、自定义事件时有时无

## 复现条件

- **复现频率**：必现（stdio 路径第 2+ 个 session）
- **触发步骤**：
  1. stdio/IDE 客户端在同一个 server 进程内创建第 2 个 session
  2. 或在 load/resume 一个未注册的 session
- **环境**：stdio transport 路径（`peri-tui/src/acp_stdio/session/create.rs:106,142,217,311` 均命中）

## 涉及文件

- `peri-acp/src/session/mod.rs:342-348` —— `consume_pending_caps` 用 `take()` 消费协商值
- `peri-acp/src/session/mod.rs:374-388` —— `ensure_session_caps` 未区分"已协商/未协商"，直接 `all_enabled()` 回退
- `peri-tui/src/acp_stdio/session/create.rs` —— stdio 路径 4 个调用点

## 修复方向（对抗 review 已确认）

- `consume_pending_caps` 改为 clone（或保留 negotiated 字段），`ensure_session_caps` 先查 `pending_caps_was_set()`：协商过则用协商值 clone，未协商才 `all_enabled`
- **必须保留双 fallback 语义**：consume 未协商 → 全 false（`unwrap_or_default`）与 ensure 未协商 → all_enabled 不同，改坏任一侧都会翻转 TUI/stdio 行为
- 顺带评估两个函数并存的冗余（`create.rs:106` 与 `:144` 对同一 session 走不同取值路径）
- 测试：stdio 集成测试断言"第 2+ 个 session/new 拿到协商值"（`stdio_test.rs` 有框架）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-acp 审查发现，对抗 review 验证） |
| 2026-08-05 | Open | Fixed | agent | 修复：consume_pending_caps 改 clone 保留协商值；ensure_session_caps 先查 pending_caps_was_set()，协商过用协商值、未协商才 all_enabled；双 fallback 语义保留 |

## 修复记录

### 修复 #1（2026-08-05）

- **操作人**：agent（Slice 1 编码切片，auto-devflow）
- **用户原意**：修复 stdio 第 2+ 个 session 拿不到 initialize 协商的 caps（全 false / all_enabled 门控错乱），同时不得翻转 TUI/stdio 任一侧的 fallback 行为
- **修复内容**：
  - **文件**：`peri-acp/src/session/mod.rs`
    - `consume_pending_caps`（:342-348）：`take()` 改为 `clone()`——协商值是 server 进程级配置（initialize 只调用一次），必须保留供第 2+ 个 session 复用；未协商仍 `unwrap_or_default()`（全 false，fallback 语义 A 不变）
    - `ensure_session_caps`（:374-388）：改为先查 `pending_caps_was_set()`——协商过用协商值 clone（`pending_caps` 只 set 从不 clear，无 TOCTOU），未协商才 `all_enabled()`（fallback 语义 B 不变）
    - `pending_caps` 字段注释同步更新（"取出清空" → "clone 保留"）
  - **测试**：`peri-acp/src/session/mod_test.rs` 新增 `test_pending_caps_consumed_once_second_session_gets_negotiated`（第 2 个 session/new + load 路径均拿协商值、registry 幂等）与 `test_pending_caps_double_fallback_semantics`（未协商时 consume=全 false、ensure=all_enabled）
  - **顺带评估**（未改动）：`create.rs:106`（consume）与 `:144`（ensure）对同一 session 走不同取值路径的冗余——两函数 fallback 语义不同（全 false vs all_enabled）系有意设计（P0-3），不合并
- **验证状态**：待验证（build ✅ / peri-acp lib 415 tests ✅，含 2 个新测试）
