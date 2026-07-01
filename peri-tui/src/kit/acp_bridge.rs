//! ACP 事件 → Atom 桥接后台 task。
//!
//! 从 EventRx 接收 ACP 事件，经 state_machine 处理后写入全局 Atom。
//! Phase 4 编译桩——函数体留空，待后续集成。

use crate::runtime::event_channel::EventRx;
use tokio_util::sync::CancellationToken;

/// 启动 ACP 事件桥接后台任务。
///
/// 从 EventRx 接收 ACP 事件（`TuiEvent::AcpEvent`），
/// 经 state_machine 转换后写入全局 Atom（ACP_STATE / VIEW_MODELS），
/// 触发组件重渲染。
///
/// 当前为编译桩——返回空的 JoinHandle。
pub fn spawn_acp_bridge(
    _rx: EventRx,
    _shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    // Phase 4 编译桩：空白任务，立刻完成
    tokio::spawn(async {})
}
