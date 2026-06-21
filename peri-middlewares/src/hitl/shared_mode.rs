use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

/// 权限模式枚举，控制 HITL 审批行为
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[derive(Default)]
pub enum PermissionMode {
    /// 所有敏感工具弹窗审批（默认）
    #[default]
    Default = 0,
    /// 允许文件系统的编辑
    AcceptEdit = 2,
    /// 大模型自动判断允不允许
    AutoMode = 3,
    /// 所有都允许
    Bypass = 4,
}

impl PermissionMode {
    /// 循环切换到下一个模式：Default → AcceptEdit → AutoMode → Bypass → Default
    pub fn next(self) -> Self {
        match self {
            Self::Default => Self::AcceptEdit,
            Self::AcceptEdit => Self::AutoMode,
            Self::AutoMode => Self::Bypass,
            Self::Bypass => Self::Default,
        }
    }

    /// 状态栏显示文本
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Default => "",
            Self::AcceptEdit => "Accept Edit",
            Self::AutoMode => "Auto Mode",
            Self::Bypass => "Bypass",
        }
    }
}

/// TryFrom<u8> 实现：异常值（>4）回退到 Default。
/// 值 1 保留为历史 DontAsk 的占位（已删除），映射到 Default 以保证向前兼容。
#[allow(clippy::fallible_impl_from)]
impl From<u8> for PermissionMode {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Default,
            2 => Self::AcceptEdit,
            3 => Self::AutoMode,
            4 => Self::Bypass,
            _ => Self::Default,
        }
    }
}

/// 跨线程共享的权限模式状态（Arc<AtomicU8> 封装）
pub struct SharedPermissionMode {
    inner: AtomicU8,
}

impl SharedPermissionMode {
    /// 创建新的共享权限模式实例，返回 Arc<Self>
    pub fn new(mode: PermissionMode) -> Arc<Self> {
        Arc::new(Self {
            inner: AtomicU8::new(mode as u8),
        })
    }

    /// 读取当前权限模式
    pub fn load(&self) -> PermissionMode {
        let v = self.inner.load(Ordering::Relaxed);
        PermissionMode::from(v)
    }

    /// 设置权限模式
    pub fn store(&self, mode: PermissionMode) {
        self.inner.store(mode as u8, Ordering::Relaxed);
    }

    /// CAS 循环切换到下一个模式，返回切换后的模式
    pub fn cycle(&self) -> PermissionMode {
        loop {
            let current = self.inner.load(Ordering::Relaxed);
            let current_mode = PermissionMode::from(current);
            let next_mode = current_mode.next();
            let next = next_mode as u8;
            match self
                .inner
                .compare_exchange(current, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return next_mode,
                Err(_) => continue,
            }
        }
    }
}

#[cfg(test)]
#[path = "shared_mode_test.rs"]
mod tests;
