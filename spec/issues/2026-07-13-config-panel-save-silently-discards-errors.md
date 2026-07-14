# Config 面板保存失败时无错误提示、UI 仍显示修改成功

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-13

## 问题描述

在 Config 面板中切换任意配置项（diff_enabled、streaming_mode、language 等），面板界面立即显示更改。但重启程序后，配置回退为修改前的旧值。**内存中的值确实被修改了，UI 也正确显示了变更，但磁盘上的 `~/.peri/settings.json` 没有被更新。**

原因是 `config::save()` 调用可能因文件权限、磁盘满、序列化失败等原因失败，而这些错误使用 `let _ = ...` 被**完全丢弃**，用户得不到任何失败提示。

## 症状详情

### 现象 1：切换配置后重启恢复旧值

| 步骤 | 期望 | 实际 |
|------|------|------|
| 1. 打开 Config 面板，将 `streaming_mode` 从 `text` 切换为 `tool_call` | 面板显示 `tool_call` | 面板显示 `tool_call` ✓ |
| 2. 退出程序 | — | — |
| 3. 重新启动程序，打开 Config 面板 | `streaming_mode` 仍为 `tool_call` | `streaming_mode` 回到 `text` ✗ |

### 现象 2：用户无任何失败反馈

| 操作 | 期望 | 实际 |
|------|------|------|
| 按 Space/Enter 切换配置项 | 到切换成功或失败提示 | 无论保存成功与否，面板行为完全一致，无任何区别 |

### 现象 3：i18n 提示字符串已定义但未使用

`peri-tui/locales/zh-CN/main.ftl:53` 已定义了 `config-save-failed = 配置保存失败: { $error }` 字符串，`peri-tui/locales/en/main.ftl:54` 定义了英文版本。Config 面板中从未调用这些字符串，也没有任何错误提示的展示逻辑。

## 代码定位

Config 面板中 **4 处**保存调用均丢弃错误，位于 `peri-tui/src/kit/panels/config.rs`：

| 行号 | 配置项 | 代码 |
|------|--------|------|
| 319 | Toggle（diff_enabled / cache_warning / context_1m） | `let _ = crate::config::save(&cfg_snapshot);` |
| 335 | streaming_mode | `let _ = crate::config::save(&snap);` |
| 342 | language | `let _ = crate::config::save(&snap);` |
| 351 | active_alias | `let _ = crate::config::save(&snap);` |

同样的问题也存在于 `peri-tui/src/kit/panels/login.rs:194`。

对比：ACP server 端的配置保存（`peri-tui/src/acp_server/requests.rs:26-31`）**正确**地使用了 `tracing::warn!` 记录了错误。

## 复现条件

- **复现频率**：条件触发（仅在 `config::save()` 实际失败时）
- **触发场景**：
  1. `~/.peri/` 目录权限不足（如 root 拥有的目录）
  2. 磁盘满
  3. 跨文件系统操作导致 `rename` 失败
  4. `serde_json::to_string_pretty` 序列化异常（罕见）
- **环境**：所有环境

## 涉及文件

| 文件 | 当前状态 | 说明 |
|------|----------|------|
| `peri-tui/src/kit/panels/config.rs:319,335,342,351` | ✅ 已修复 | 4 处改为 match 块：成功→NOTIFICATION("config-saved" 3s)，失败→NOTIFICATION("config-save-failed" 5s) |
| `peri-tui/src/kit/panels/login.rs:194` | ✅ 已修复 | 同理改为 match 块 + NOTIFICATION 反馈 |
| `peri-acp/src/provider/store.rs:52-68` | 参考 | `save()` / `save_to()` 实现，了解可能的失败原因 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建 |
| 2026-07-13 | Open | Fixed | agent | 修复：5 处 `let _ = save(...)` → match + NOTIFICATION 反馈 |

## 修复记录

### 修复 #1（2026-07-13）

- **操作人**：agent
- **修复内容**：
  1. `config.rs` 4 处 `let _ = crate::config::save(...)` → match 块：成功写入 NOTIFICATION 3s "配置已保存"，失败写入 NOTIFICATION 5s "配置保存失败: {error}"
  2. `login.rs` 1 处同类替换
  3. 新增 imports：`fluent_bundle::FluentValue`、`std::time::{Duration, Instant}`、`Notification`/`NOTIFICATION` atom
- **涉及文件**：`config.rs`（+72/-5）、`login.rs`（+20/-2）
- **验证**：cargo build/test/clippy 全通过，无残留 `let _ = save` 调用
- **验证状态**：待用户手动验证
