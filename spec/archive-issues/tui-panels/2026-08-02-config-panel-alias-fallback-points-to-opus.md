> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-config-panel-alias-fallback-points-to-opus.md

# config 面板 active_alias 空值回退索引指向 opus 而非注释声称的 sonnet

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-02

## 问题描述

配置面板的 `read_cycle_idx` 对空/未知 `active_alias` 硬编码返回索引 1（注释写 "default sonnet"）。fable 加入 `ALIAS_OPTS` 首位后，索引 1 是 **opus** 而非 sonnet——未设置别名时面板高亮 opus，与注释意图及实际默认不符。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `peri-tui/src/kit/panels/config.rs` 约 56 行：`ALIAS_OPTS = ["fable", "opus", "sonnet", "haiku"]`。
- 约 328-332 行：`if cur.is_empty() { 1 } // default sonnet`；未知值 `unwrap_or(1)`——均指向 opus。
- 面板以高亮项暗示当前别名；用户未配置别名时看到的是 opus，且循环切换到 fable 需多按一次键。

## 复现条件

- **复现频率**：必现（active_alias 为空或非法时）
- **触发步骤**：
  1. 打开配置面板（/config 或等价入口），确保 active_alias 为空或未知
  2. 观察别名行高亮在 opus 上
- **环境**：任意 active_alias 缺失的配置

## 期望改进方向

- 回退改为按名称查找 `"sonnet"` 的索引（找不到才用 0），同时覆盖空值与未知值两个分支。

## 涉及文件

- `peri-tui/src/kit/panels/config.rs` —— `ALIAS_OPTS`（约 56 行）与 `read_cycle_idx`（约 305-353 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: read_cycle_idx 回退改为按名称查找 "sonnet" 索引，消除硬编码 1 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：read_cycle_idx 按名称查索引替代硬编码（config.rs:338），fable 加入后不再漂移 |

## 修复记录

- 改动：`peri-tui/src/kit/panels/config.rs` `read_cycle_idx` 的 `ROW_ACTIVE_ALIAS` 分支——用 `options.iter().position(|o| *o == "sonnet").unwrap_or(0)` 计算默认索引（空值与未知值两个回退分支共用），删除硬编码 `1`。ALIAS_OPTS 顺序变化不再导致默认高亮漂移。
- 验证：`cargo check -p peri-tui --all-targets` 通过（7.61s，无警告）。
