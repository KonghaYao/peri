# Login 面板缺少 Provider 类型编辑字段

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-17

## 问题描述

Login 面板（`/login`）的编辑模式中，`LoginEditField` 枚举缺少 `ProviderType` 字段。用户无法在编辑 provider 时修改类型（anthropic ↔ openai-compatible），所有 provider 创建后类型被锁定。

## 症状详情

| 项目 | 详情 |
|------|------|
| 触发条件 | `/login` → Enter 选中某 provider → E 进入编辑模式 |
| 当前行为 | 可编辑字段：ProviderId、ApiKey、BaseUrl、OpusModel、SonnetModel、HaikuModel。**没有 Type 字段** |
| 期望行为 | 应有 Type 字段，支持在 anthropic / openai-compatible 之间切换（类似 Setup Wizard 中 Left/Right 或 Space 切换） |

## 涉及文件

- `peri-tui/src/kit/panels/login.rs:45-87` —— `LoginEditField` 枚举（缺少 `ProviderType` 变体）
- `peri-tui/src/kit/panels/login.rs:90-113` —— `LoginEditState` 结构体（缺少 `provider_type` 字段）
- `peri-tui/src/kit/panels/login.rs:706-750` —— `save_login_edit()` 函数（未保存 `provider_type`）
- `peri-tui/src/kit/panels/login.rs:526-542` —— `enter_login_edit_mode()` 函数（未读取 `provider_type`）
- `peri-acp/src/provider/config.rs:278-299` —— `ProviderConfig` 定义（`provider_type: String`，序列化为 `"type"`）

## 参考

Setup Wizard（`setup_wizard.rs`）已正确实现 ProviderType 字段——`FormField::ProviderType` 支持 Left/Right 或 Space 切换。Login 面板可参考相同交互模式。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | - | Open | agent | 创建 |
| 2026-07-17 | Open | Fixed | agent | LoginEditField 新增 ProviderType + toggle 交互 + hint 提示 |

## 修复记录

### 修复 #1（2026-07-17）

- **操作人**：agent
- **用户原意**：Login 面板编辑模式增加 ProviderType 切换字段，编辑模式底部 hint 行增加 ←/→ 切换提示
- **修复内容**：
  - `LoginEditField` 枚举新增 `ProviderType` 变体（首位），`next()`/`prev()`/`i18n_key()` 同步
  - `LoginEditState` 新增 `provider_type: String` 字段，`field_value()`/`field_value_mut()` 同步
  - ProviderType 渲染为 toggle：显示 `[Anthropic]`/`[OpenAI Compatible]`，`←`/`→`/`Space` 切换
  - `save_login_edit()` 保存 `provider_type` 到配置，`enter_login_edit_mode()` 初始化读取
  - 编辑模式底部 hint 行增加 `←/→ :切换` 提示
- **涉及 commit**：待提交
- **验证状态**：待验证
