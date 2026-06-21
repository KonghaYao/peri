//! PermissionRequest hook 门控逻辑。
//!
//! [TRAP] PermissionRequest 双条件门控：`requires_approval(tool_name)`
//! **与** `needs_permission_dialog(tool_name)`，顺序不能颠倒（短路优化 +
//! 语义对齐 Claude Code）。AutoMode 始终触发 PermissionRequest 的特殊处理
//! 不能丢。
//!
//! 对齐 Claude Code：PermissionRequest 仅在权限对话框即将展示给用户时触发。
//! - Bypass: 所有工具直接放行，无对话框
//! - AcceptEdit: 编辑工具放行，其他弹窗
//! - AutoMode: 分类器决定；为避免 hook 系统依赖分类器，AutoMode 下始终触发
//! - Default: 敏感工具始终弹窗

use crate::hitl::PermissionMode;

/// 判断当前权限模式下，给定工具是否会触发权限对话框。
///
/// 该函数仅描述"权限模式是否会导致弹窗"，不与 `requires_approval` 合并：
/// 调用方必须先调用 `requires_approval(tool_name)`，再调用本函数（见
/// `should_fire_permission_request`）。
pub fn needs_permission_dialog(mode: PermissionMode, tool_name: &str) -> bool {
    match mode {
        // Bypass: 所有工具直接放行，无对话框
        PermissionMode::Bypass => false,
        // AcceptEdit: 编辑工具放行，其他弹窗
        PermissionMode::AcceptEdit => !crate::hitl::is_edit_tool(tool_name),
        // AutoMode: 分类器决定；简化处理——当无分类器或 Unsure 时弹窗
        // 为避免 hook 系统依赖分类器，AutoMode 下始终触发 PermissionRequest
        PermissionMode::AutoMode => true,
        // Default: 敏感工具始终弹窗
        PermissionMode::Default => true,
    }
}

/// 综合判断是否应该触发 PermissionRequest hook：
/// `requires_approval(tool_name) && needs_permission_dialog(mode, tool_name)`。
///
/// [TRAP] 顺序固定：先判断 `requires_approval`，短路后再判断
/// `needs_permission_dialog`。若颠倒顺序，Bypass 模式下仍会为敏感工具触发
/// PermissionRequest，违反"权限对话框即将展示时才触发"的语义。
pub fn should_fire_permission_request(
    mode: PermissionMode,
    tool_name: &str,
    requires_approval: fn(&str) -> bool,
) -> bool {
    let is_sensitive = requires_approval(tool_name);
    if !is_sensitive {
        return false;
    }
    needs_permission_dialog(mode, tool_name)
}
