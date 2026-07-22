# 删除 YOLO_MODE 环境变量，全部改用 PermissionMode 系统

**状态**：已修复
**优先级**：中
**创建日期**：2026-07-21
**修复日期**：2026-07-21

## 问题描述

`YOLO_MODE` 是最初设计的环境变量权限开关，现在已经完全被 `PermissionMode`（Bypass/Default/AcceptEdit/AutoMode）系统替代。但代码中仍有 16 处 `YOLO_MODE` 引用，分布在 6 个文件中，造成了"同一概念两个来源"的混乱。需要全部删除对 `YOLO_MODE` 环境变量的读取，将相关逻辑统一到 permission 系统。

## 现状

### YOLO_MODE 在代码中的分布

| 文件 | 引用数 | 角色 |
|------|--------|------|
| `peri-middlewares/src/hitl/mod.rs` | 7 处 | `is_yolo_mode()` 核心函数 + `from_env()` 构造 + 注释 |
| `peri-tui/src/launch.rs` | 2 处 | 启动时 YOLO_MODE 作为 permission mode fallback |
| `peri-tui/src/main.rs` | 2 处 | `--approve` → `set_var("YOLO_MODE", "false")` 等 CLI 映射 |
| `peri-acp/src/prompt/mod.rs` | 1 处 | `PromptFeatures::detect()` 读 YOLO_MODE 决定 `hitl_enabled` |
| `peri-acp/src/prompt/prompt_test.rs` | 2 处 | 测试注释 |
| `peri-acp/src/agent/builder.rs` | 1 处 | 注释 |
| `peri-tui/src/kit/panels/config.rs` | 1 处 | 注释 |

### 当前 PermissionMode 系统

Permission 系统已经完整覆盖了 YOLO_MODE 的所有功能：

- **枚举**：`PermissionMode::Bypass`（对应旧 YOLO）、`Default`（对应旧审批）、`AcceptEdit`、`AutoMode`
- **初始化**：CLI `--permission-mode` > `--approve` > `--skip-permissions` > 环境 fallback
- **运行时切换**：`Shift+Tab` 循环切换，通过 `SharedPermissionMode`（AtomicU8）
- **工具审批**：HITL 中间件 `decide_by_mode()` 统一决策
- **ACP 协议**：`session/new` 和 `setSessionMode` 支持 `permission_mode` 参数

## 期望改进方向

删除对 `YOLO_MODE` 环境变量的所有读取和写入，所有 permission 判定统一走 `PermissionMode`：

1. 删除 `is_yolo_mode()` 函数和 `HumanInTheLoopMiddleware::from_env()` 构造
2. `PromptFeatures::hitl_enabled` 改为从 `SessionConfig::permission_mode` 获取（而非读环境变量）
3. `main.rs` 中删除 `set_var("YOLO_MODE", ...)`   改用 CLI 参数直接设置 `PermissionMode`
4. `launch.rs` 中删除 YOLO_MODE fallback 逻辑
5. 更新所有相关注释

## 涉及文件

- `peri-middlewares/src/hitl/mod.rs`（424 行）—— 核心：`is_yolo_mode()` 和 `from_env()` 定义
- `peri-tui/src/main.rs` —— CLI `set_var` 映射
- `peri-tui/src/launch.rs` —— 启动时 YOLO_MODE fallback
- `peri-acp/src/prompt/mod.rs`（334 行）—— `PromptFeatures::detect()`
- `peri-acp/src/agent/builder.rs` —— 注释清理
- `peri-acp/src/prompt/prompt_test.rs` —— 测试注释清理
- `peri-tui/src/kit/panels/config.rs` —— 注释清理
- `peri-acp/src/session/state_builders.rs` —— 可能需要新增 `hitl_enabled` 从 permission mode 派生的逻辑

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-21 | — | Open | agent | 创建 |
| 2026-07-21 | Open | 已修复 | agent | 所有 YOLO_MODE 引用已清理 |

## 修复记录

### 修改文件（12 个）

| 文件 | 修改内容 |
|------|----------|
| `peri-acp/src/prompt/mod.rs` | `detect()` 添加 `PermissionMode` 参数，`hitl_enabled = (mode != Bypass)` |
| `peri-acp/src/prompt/prompt_test.rs` | 所有 `detect()` 调用传 `PermissionMode::Bypass` |
| `peri-acp/src/agent/builder.rs` | `detect(permission_mode.load())` + 注释更新 |
| `peri-acp/src/agent/workflow_agent.rs` | `detect(PermissionMode::Bypass)` |
| `peri-acp/src/session/executor.rs` | `FrozenSessionData::build()` 添加 `permission_mode` 参数 |
| `peri-acp/src/session/mod.rs` | `build_frozen_data` 读取 `permission_mode.load()` |
| `peri-tui/src/cli_print.rs` | `build()` 传 CLI 解析的 `permission_mode` |
| `peri-tui/src/main.rs` | 删除两个 `set_var("YOLO_MODE", ...)` unsafe 块 |
| `peri-tui/src/launch.rs` | 删除 YOLO_MODE fallback，统一用 CLI 参数 |
| `peri-middlewares/src/hitl/mod.rs` | 删除 `is_yolo_mode()` + `from_env()`，更新注释 |
| `peri-middlewares/src/lib.rs` | 删除 `is_yolo_mode` 两处导出 |
| `peri-agent/src/interaction/mod.rs` | doc example 更新 |
| `CLAUDE.md` | 更新 YOLO_MODE 相关文档 |

### 验证结果
- `cargo build --workspace`: ✅ 零错误零警告
- `cargo test -p peri-acp --lib`: ✅ 294 passed
- `cargo test -p peri-middlewares --lib`: ✅ 994 passed
- `rg --type rust YOLO_MODE`: ✅ 零命中
