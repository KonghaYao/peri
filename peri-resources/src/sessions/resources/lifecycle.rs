//! 关闭生命周期：`Open → Closing → Closed`，只有**确认关闭**才是 `Closed`。
//!
//! 关闭有三件必须分开的事实：
//!
//! 1. **停止新写入不可逆**：进入 `Closing` 后不再接受新的 mutation 准入（`ensure_open`
//!    按「只有 `Open` 放行」判定），但这不是「已关闭」；
//! 2. **未结清事实仍要能收敛**：`Closing` 保留恢复与排空权限（见
//!    [`super::gate::MutationGate::ensure_recovery_permitted`]），否则第一次关闭失败就把
//!    唯一能推进未决的路径也关掉了；
//! 3. **`Closed` 只是确认的结果**：只有在真实检查（在途写入、未决锚点）全部结清、并且
//!    数据面确实关闭之后才允许迁移。失败或取消的关闭停在 `Closing`，重复关闭必须重新
//!    检查，不允许用「曾经调用过」冒充幂等成功。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const OPEN: u8 = 0;
const CLOSING: u8 = 1;
const CLOSED: u8 = 2;

/// 资源生命周期状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LifecycleState {
    Open,
    Closing,
    Closed,
}

/// 门面与写入闸门共享的生命周期事实。
#[derive(Clone)]
pub(super) struct Lifecycle {
    state: Arc<AtomicU8>,
}

impl Lifecycle {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(OPEN)),
        }
    }

    pub(super) fn state(&self) -> LifecycleState {
        match self.state.load(Ordering::Acquire) {
            CLOSING => LifecycleState::Closing,
            CLOSED => LifecycleState::Closed,
            _ => LifecycleState::Open,
        }
    }

    /// 停止新写入（不可逆）。`Open → Closing`；已在 `Closing`/`Closed` 的原样返回。
    pub(super) fn begin_closing(&self) -> LifecycleState {
        let _ = self
            .state
            .compare_exchange(OPEN, CLOSING, Ordering::AcqRel, Ordering::Acquire);
        self.state()
    }

    /// 确认关闭：只有 `Closing` 能成为 `Closed`，返回本次是否完成了迁移。
    pub(super) fn confirm_closed(&self) -> bool {
        self.state
            .compare_exchange(CLOSING, CLOSED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}
