//! Permission DTOs -- 取代 peri_middlewares::prelude::PermissionMode

use serde::{Deserialize, Serialize};

/// 权限模式（对齐 peri_middlewares::prelude::PermissionMode）
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PermissionModeDto {
    /// 所有敏感工具弹窗审批（默认）
    Default,
    /// 允许文件系统的编辑
    AcceptEdit,
    /// 大模型自动判断允不允许
    AutoMode,
    /// 所有都允许
    Bypass,
}
