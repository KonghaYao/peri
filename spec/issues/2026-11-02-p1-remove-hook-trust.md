# P1：移除全部 hook trust 校验

**状态**：已实施，待验收。
**优先级**：P1。
**类型**：安全 / 行为变更。
**创建日期**：2026-11-02。
**来源**：用户明确决定（不需要 trust 功能，全部删除，让 hooks 通过）。
**确认方式**：用户决策 + 主 agent 代码盘点（基线 `1655935ae`）。

## 决策

删除全部 hook 执行来源信任功能：

- 项目 / local settings hooks 不再要求 workspace 授权，直接装配执行。
- 插件来源 hooks 不再要求插件来源授权，直接装配执行。
- 删除 `peri plugin trust` CLI、`hook-trust.json` 读写、`hook trust` 模块与相关测试/文档。
- 保留 `SettingsHooksPort` 端口、`PluginScope` 来源身份与 hook 分组逻辑；它们不依赖 trust。

## 确认证据（当前代码）

- 信任模块：`peri-config/src/trust.rs`（`HookTrustEntry`、`settings_binding`、`is_trusted`、`grant`、`revoke`、`list`、`hook-trust.json`）。
- settings hooks 准入：`peri-middlewares/src/host_ports.rs:544-592`（`admit_settings_hooks`），`SettingsHooksLoader` 在 `project` / `local` 调用它。
- 插件 hooks 绑定/准入：`peri-middlewares/src/host_ports.rs:603-726`（`plugin_hook_binding` / `admit_plugin_hooks`），装配点 `peri-acp/src/host/assemble.rs:805-809`。
- CLI：`peri-tui/src/cli_plugin.rs:532-706`（`run_plugin_trust_grant/revoke/status` 等），命令定义 `peri-tui/src/main.rs:278-308` 与分发 `main.rs:904-912`。
- 测试：`peri-config/src/trust_test.rs`、`peri-middlewares/src/host_ports_test.rs` 的 H4/plugin trust 用例、`peri-middlewares/src/plugin/loader_admission_test.rs:81-136`。

## 删除范围

### 代码

1. 删除 `peri-config/src/trust.rs`、`peri-config/src/trust_test.rs`；移除 `peri-config/src/lib.rs` 的 `pub mod trust;`；移除 `peri-config/Cargo.toml` 的 `sha2` 依赖（仅 trust 使用）。
2. `peri-middlewares/src/host_ports.rs`
   - 删除 `admit_settings_hooks`；`SettingsHooksLoader::project/local` 直接返回 `load_settings_project_hooks` / `load_settings_local_hooks`。
   - 删除 `plugin_hook_binding`、`admit_plugin_hooks` 及相关常量/注释。
3. `peri-acp/src/host/assemble.rs`
   - 删除 `admit_plugin_hooks` 调用与 M6/H4 注释；`plugin_hooks` 直接进入 `assemble_hook_groups`。
4. `peri-tui/src/cli_plugin.rs`
   - 删除 H4 信任段：`settings_trust_scope`、`trust_workspace`、`trust_binding`、`enabled_plugins_for_trust`、`resolve_plugin_for_trust`、`plugin_trust_binding`、`run_plugin_trust_grant/revoke/status`，以及 `use peri_config::trust...`。
5. `peri-tui/src/main.rs`
   - 删除 `PluginAction::Trust`、`TrustAction` 枚举与分发分支。

### 测试

1. 删除 `peri-config/src/trust_test.rs`。
2. `peri-middlewares/src/host_ports_test.rs`
   - 删除 H4 信任用例、`GlobalConfigGuard`、`PROJECT_HOOK*`、`write_project_settings`、`plugin_hook_trust` 模块。
   - 新增最小回归：项目/local settings hooks 在无任何信任记录时直接加载（写 `.claude/settings.json` / `settings.local.json`，断言 `SettingsHooksLoader::project/local` 返回 1 条）。
   - 保留 `bind_agent_catalog_from_pool` 等无关测试。
3. `peri-middlewares/src/plugin/loader_admission_test.rs`
   - 删除 trust 配置 guard/grant/admit 调用；保留并重命名“共享根目录两顺序下 loader 保留插件来源身份”的断言（不涉及授权）。
4. 搜索并清理所有残留引用（`HookTrustEntry`、`hook-trust`、`admit_plugin_hooks`、`plugin_hook_binding`、`run_plugin_trust`、`TrustAction` 等）。

### 文档

1. `docs/code-index/peri-config.md`：删除 Hook trust 摘要行。
2. `docs/code-index/peri-middlewares.md`：删除插件 trust 门控行；修正 hook 装配行（去掉“含信任准入”）；重写插件来源身份行（只保留分组/身份语义）。
3. `docs/code-index/peri-tui.md`：删除 `peri plugin trust` CLI 行。
4. `docs/code-index/peri-acp-types.md`：插件来源契约行去掉 trust 消费方，保留 `PluginScope` 与来源身份。
5. `spec/issues/2026-10-07-p1-agent-context-injection-preflight.md`：H4 信任门条目追加 2026-11-02 决策说明，标记 trust 已移除，引用本 issue；保留原文作为历史。
6. 全仓搜索 `trust`、`hook-trust`、`H4 信任`，清理仍在描述现行行为的段落；历史 spec 只加“已废弃/已移除”说明，不删除历史上下文。

## 明确不做

- 不恢复/新增任何信任交互或授权开关。
- 不改变 hook 的执行顺序、事件、超时、输出解析与 `SettingsHooksPort` 端口形状。
- 不删除 `PluginScope` / `source_identity` / hook 分组匹配。
- 不在本 issue 处理插件 `hookSpecificOutput` 大小写兼容问题（另立任务）。

## 验收标准

1. 在没有任何 `hook-trust.json` / 授权记录的 workspace 中，项目/local settings hooks 直接装配执行。
2. 插件声明 hooks 时直接装配执行，不再有未信任跳过日志。
3. `peri plugin trust` 子命令不存在（`peri plugin --help` 不再列出 `trust`）。
4. 全仓无 `HookTrustEntry` / `hook-trust` / `admit_plugin_hooks` / `plugin_hook_binding` / `run_plugin_trust` 引用。
5. 目标检查：`cargo test -p peri-config --lib`、`cargo test -p peri-middlewares --lib -- host_ports`、`cargo test -p peri-middlewares --lib -- plugin::`、`cargo test -p peri-acp --lib` 相关用例、`cargo test -p peri-tui --bin peri -- cli_plugin::tests::`、`bash scripts/check-changed.sh`、`git diff --check`。

## 验证记录

实施环境：worktree `.worktrees/remove-hook-trust`（基线 `1655935ae`），2026-11-02。

### 删除与保留

- 删除 `peri-config/src/trust.rs`、`trust_test.rs` 及 `pub mod trust;`。
- `peri-config/Cargo.toml` 的 `sha2` **保留**：`peri-config/src/system.rs` 仍在使用
  `Sha256`，并非仅 trust 使用。
- `SettingsHooksLoader::project/local` 直接返回 loader 结果；`admit_settings_hooks`、
  `plugin_hook_binding`、`admit_plugin_hooks` 已删除。
- `PluginScope`、`source_identity()`、`SettingsHooksPort`、hook 分组/匹配逻辑保留。
- `peri-tui` 的 `PluginAction::Trust`、`TrustAction` 与 trust 子命令实现删除；
  `peri plugin --help` 已无 `trust`（实际执行确认）。

### 命令与结果

1. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-config --lib`
   → `186 passed; 0 failed; 1 ignored`。
2. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- host_ports`
   → `3 passed; 0 failed`（含新增 `settings_hooks_load_without_any_trust_record`）。
3. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- plugin::`
   → `198 passed; 0 failed`；重命名用例
   `real_loader_preserves_hook_source_identity_for_shared_roots_in_both_orders` 单独运行通过。
4. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- assemble`
   → `6 passed; 0 failed`。
5. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --bin peri -- cli_plugin::tests::`
   → `6 passed; 0 failed`。
6. `bash scripts/check-changed.sh` → 退出码 0。
7. `bash scripts/check-file-size.sh` → 报告 20 个既有超限文件；本次改动文件均未超限
   （最大 `peri-acp/src/host/assemble.rs` 922 行）。
8. `git diff --check` → 退出码 0。
9. `cargo fmt --all -- --check` → 退出码 0。
10. 残留检索
    `rg -n "HookTrustEntry|hook-trust|HOOK_TRUST_FILE|settings_binding|plugin_hook_binding|admit_plugin_hooks|admit_settings_hooks|run_plugin_trust|TrustAction|peri_config::trust|trust_store_path"`
    → 仅剩本 issue、`2026-10-07` 的已移除去历史说明，以及
    `spec/issues/2026-10-12-mcp-apps-redesign/implementation-log.md` 中描述当时测试约束的
    历史记录（未改写）。