//! Permission DTOs -- 取代 peri_middlewares::prelude::PermissionMode

use serde::{Deserialize, Serialize};

/// 权限模式（对齐 peri_middlewares::prelude::PermissionMode）
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PermissionModeDto {
    Default,
    AcceptEdits,
    Plan,
    Yolo,
}
