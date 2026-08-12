> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-config-update-missing-active-alias-validation.md

# session/update_config 缺少 active_alias 合法性校验，非法别名被持久化后静默失效

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`session/update_config` 的校验循环逐个检查各 profile 引用的 provider 是否存在，但**不检查** `config.active_alias` 本身是否是 `Profiles::ALL` 中的固定键。非法 active_alias 被持久化后，后续依赖 active_alias 的处理器（如 `context_1m`）会静默 no-op，行为不可见。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `peri-tui/src/acp_stdio/session/config.rs` 约 127-139 行：只校验 `profiles[alias].provider` 是否存在于 providers 列表。
- `active_alias` 无成员检查：`new_cfg.config.active_alias` 可以是任意字符串并成功持久化（约 141 行 `*ctx.peri_config.write() = new_cfg`）。
- 后果链路：非法 active_alias → 后续 `profiles.get_mut(&alias)` 返回 None → 配置写入静默丢弃。

## 复现条件

- **复现频率**：外部客户端传入非法 active_alias 时
- **触发步骤**：
  1. 调用 `session/update_config`，`config.active_alias` 设为未知值（如 `"foo"`）
  2. 观察请求成功返回；随后对该配置执行 `context_1m` 等操作无效果
- **环境**：任意 ACP 客户端（含第三方）

## 期望改进方向

- 在现有 provider 引用校验旁增加 `active_alias ∈ Profiles::ALL`（大小写不敏感）成员检查，非法时返回 invalid_request 错误。

## 涉及文件

- `peri-tui/src/acp_stdio/session/config.rs` —— 配置校验块（约 123-139 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: update_config 增加 active_alias ∈ Profiles::ALL 大小写不敏感校验，非法值返回 invalid_request |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：active_alias 合法性校验补齐，修复记录见正文 |

## 修复记录

- 改动：`peri-tui/src/acp_stdio/session/config.rs` `handle_update_config`——在 provider 引用校验循环后新增 `Profiles::ALL.iter().any(|a| a.eq_ignore_ascii_case(...))` 成员检查（大小写不敏感，与 `Profiles::get` 行为一致），非法 active_alias 返回 `invalid_request`，不再静默持久化。
- 验证：`cargo check -p peri-tui --all-targets` 通过（7.61s，无警告）。
