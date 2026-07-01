//! ACP 事件 → Atom 桥接后台 task。
//!
//! 从 mpsc::UnboundedReceiver 接收已解码的 ACP 事件，
//! 经 acp_events::dispatch_and_notify 处理后写入全局 Atom。
//! Phase 2 完整实现——main_loop fan-out 后独立消费。

use crate::kit::acp_events::{self, BridgeState};
use crate::state_machine::current_turn::CurrentTurn;
use crate::state_machine::event::AcpEventData;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 启动 ACP 事件桥接后台任务。
///
/// 从独立的 mpsc::UnboundedReceiver 读取 ACP 事件（main_loop 会 fan-out），
/// 维护 BridgeState 内部状态，每次事件后写入 VIEW_MODELS / ACP_STATE Atom，
/// 触发 ratatui-kit 组件重渲染。
pub fn spawn_acp_bridge(
    mut rx: mpsc::UnboundedReceiver<AcpEventData>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut state = BridgeState {
            variant: 0,
            committed: Vec::new(),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_active: false,
            popup_kind: None,
        };

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                event = rx.recv() => {
                    match event {
                        None => break,
                        Some(event) => {
                            acp_events::dispatch_and_notify(&mut state, &event);
                        }
                    }
                }
            }
        }
    })
}
