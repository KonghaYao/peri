> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-context-1m-missing-profile-silently-noop.md

# context_1m 配置项在 active profile 缺失时静默 no-op 却上报"已持久化"

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`configOption` 的 `context_1m` 分支通过 `profiles.get_mut(&active_alias)` 写值：active profile 不存在时写入被静默跳过，但随后仍调用 `persist_config` 并打印 "Context 1M changed via configOption (persisted)"。响应返回未变更的旧值，客户端看到陈旧开关且无从得知失败。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `peri-tui/src/acp_server/requests.rs` 约 254-265 行：
  - `if let Some(profile) = c.config.profiles.get_mut(&alias)` 内写 `profile.context_1m`；无 else 分支。
  - `persist_config(cfg)` 与 `info!(..., "(persisted)")` 无条件执行。
- 响应通过 `make_config_options` 返回原值——与客户端预期的新值不一致。

## 复现条件

- **复现频率**：active_alias 不在 profiles 中时（如配置被外部写入非法 alias）
- **触发步骤**：
  1. 使 `config.active_alias` 指向不存在的 profile
  2. 发送 `configOption` `context_1m` 更新
  3. 观察日志声称已持久化，但响应值未变化
- **环境**：非法/不一致配置状态

## 期望改进方向

- profile 缺失时记录 warning 并避免上报"已持久化"，让客户端能感知写入失败（或返回错误）。

## 涉及文件

- `peri-tui/src/acp_server/requests.rs` —— `context_1m` 分支（约 254-265 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: profile 缺失时跳过 persist 并记录 warning，不再谎报已持久化 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：active profile 缺失时显式报错而非静默 no-op，修复记录见正文 |

## 修复记录

- `peri-tui/src/acp_server/requests.rs` `context_1m` 分支：引入 `updated` 标志，仅当 `profiles.get_mut(&alias)` 命中时写值并 `persist_config` + 上报 "(persisted)"；profile 缺失时改记 `warn!("Context 1M configOption skipped: active profile not found")`，不持久化、不声称成功。
- 响应仍返回 `make_config_options` 的当前值（客户端可对照日志感知失败）。
- 验证：`cargo check -p peri-tui --all-targets` 通过（仅 2 个 setup_wizard.rs 既有 unused_mut 警告，与本次改动无关）。
