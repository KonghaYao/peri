> 归档于 2026-07-17，原路径 spec/issues/2026-07-13-model-login-panel-persistence-lost.md
# Model/Login 面板切换后重启配置丢失 + 状态栏更新延迟

**状态**：Fixed
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-13

## 问题描述

在 `/model` 或 `/login` 面板中切换模型/Provider 后，存在两个问题：

1. **持久化丢失**：会话内状态正常更新，但关闭 Peri 重新启动后切换结果丢失，回退到切换前的值。必现。
2. **状态栏更新延迟**：面板确认切换后，状态栏并未立即反映变化，延迟约 1 秒才更新。

## 期望行为

正确的逻辑应为：

- **启动时**：缓存 active provider 和 active model 到变量
- **后续改动**：所有修改操作只改这两个缓存变量，然后触发持久化存储
- **面板确认后**：状态栏应**立即**反映新值，不应有任何可感知的延迟

## 症状详情

| 面板 | 操作 | 当前行为 | 期望行为 |
|------|------|---------|---------|
| `/model` | Enter 切换 alias | 状态栏约 1s 后更新；重启后丢失 | 状态栏立即更新；重启后保持 |
| `/login` | Enter 切换 provider | 状态栏约 1s 后更新；重启后丢失 | 状态栏立即更新；重启后保持 |

## 复现条件

- **复现频率**：必现（100%）
- **触发步骤**：
  1. 启动 Peri
  2. 输入 `/model`，用 Enter 切换到一个不同的 alias
  3. 观察状态栏——模型名有约 1 秒延迟才变更
  4. 关闭 Peri 并重启——模型回退到切换前的值
  5. 输入 `/login` 切换到不同 provider，现象相同
- **环境**：macOS 26.5.1 / Rust release build

## 涉及文件

- `peri-tui/src/kit/panels/model.rs:106-138` —— Enter 切换后添加 `crate::config::save()` + NOTIFICATION 反馈
- `peri-tui/src/kit/panels/login.rs:63-86,201-234` —— 恢复同步写 PERI_CONFIG_HANDLE（保证 PROVIDER_LIST.is_active 正确）；重写 `activate_provider` 为读锁 clone → save（始终持久化，不再依赖等式守卫）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建 |
| 2026-07-13 | Open | Fixed | agent | 修复：/model 添加 save + /login 删除过早同步写 |

## 修复记录

### 修复 #1（2026-07-13）

- **操作人**：agent
- **用户原意**：/login 和 /model 面板切换后，变更应持久化到磁盘，重启后保持；状态栏应立即更新
- **修复内容**：
  - `model.rs`：在 PERI_CONFIG_HANDLE 更新后添加 `crate::config::save()` 调用（clone → drop lock → save → NOTIFICATION）。SERVICE_SNAPSHOT 推送同时设置 `model_alias` + `model_name`（通过新增 `resolve_model_name` 函数从 provider config 解析），避免状态栏显示旧模型名。
  - `login.rs`：恢复事件处理器对 PERI_CONFIG_HANDLE.active_provider_id 的同步写；新增 `PROVIDER_LIST` atom 的 `is_active` 同步刷新（该 atom 是启动时静态构建的，不会自动更新）；SERVICE_SNAPSHOT 推送同时设置 `provider_name` + `model_name`；重写 `activate_provider` 为读锁 clone → save（始终持久化）；删除不再需要的 `apply_provider_switch` 函数及 4 个测试
- **涉及 commit**：待提交
- **验证状态**：待验证
