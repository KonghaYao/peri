//! Hook 组装配（plugin → global → project → local），由 `assemble.rs` 迁出
//! （STD-SIZE-001）。顺序即迁移前 TUI/print/stdio 三处一致的既有顺序，
//! ARC-MIDDLEWARE-001 不重排。

use peri_acp_types::hooks::{RegisteredHook, SettingsHooksPort};

/// 组装 settings hook 组（plugin → global → project → local）。
///
/// `skip_settings_hooks`：bare 模式跳过 global/project/local（与 print 既有语义
/// 一致）；plugin hooks 为空时不产生空组。三级 settings hooks 经
/// `SettingsHooksPort` 注入（装配点构造，磁盘加载与其信任准入留在实现方）。
pub fn assemble_hook_groups(
    plugin_hooks: &[RegisteredHook],
    settings_hooks: &dyn SettingsHooksPort,
    cwd: &str,
    skip_settings_hooks: bool,
) -> Vec<Vec<RegisteredHook>> {
    let mut hook_groups: Vec<Vec<RegisteredHook>> = Vec::new();
    if !plugin_hooks.is_empty() {
        hook_groups.push(plugin_hooks.to_vec());
    }
    if skip_settings_hooks {
        return hook_groups;
    }
    let global_hooks = settings_hooks.global();
    if !global_hooks.is_empty() {
        hook_groups.push(global_hooks);
    }
    let project_hooks = settings_hooks.project(cwd);
    if !project_hooks.is_empty() {
        hook_groups.push(project_hooks);
    }
    let local_hooks = settings_hooks.local(cwd);
    if !local_hooks.is_empty() {
        hook_groups.push(local_hooks);
    }
    hook_groups
}
