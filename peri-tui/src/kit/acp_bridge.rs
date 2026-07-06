//! ACP 事件 → Atom 桥接后台 task。
//!
//! 从 mpsc::UnboundedReceiver 接收已解码的 ACP 事件，
//! 经 acp_events::dispatch_and_notify 处理后写入全局 Atom。
//! Phase 2 完整实现——main_loop fan-out 后独立消费。

use crate::kit::acp_events::{self, BridgeState};
use crate::kit::acp_types::{AcpEventData, CurrentTurn};
use crate::kit::atoms;
use std::sync::Arc;
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
            committed: Arc::from([]),
            current_turn: CurrentTurn::new(),
            is_loading: false,
            popup_kind: None,
            has_view_commit: false,
        };

        // 追踪 BRIDGE_RESET_COUNTER——submit_consumer 的 /clear / thread_load
        // 递增此计数器，bridge 检测到变更时立即清空 committed/has_view_commit，
        // 防止旧 session 的 ViewModel 在新 session 中残留。
        let mut last_reset_counter: u64 = 0;

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                event = rx.recv() => {
                    match event {
                        None => break,
                        Some(event) => {
                            // 在处理每个事件前检查是否需要重置 bridge 状态
                            let counter = atoms::BRIDGE_RESET_COUNTER.get();
                            if counter != last_reset_counter {
                                last_reset_counter = counter;
                                state.committed = Arc::from([]);
                                state.current_turn.reset();
                                state.has_view_commit = false;
                                state.is_loading = false;
                                state.popup_kind = None;
                                // 立即推送空快照到 VIEW_MODELS atom——
                                // 防止 render_bridge 在下一次事件到达前读到旧数据。
                                acp_events::push_view_models_for_reset();
                                tracing::info!(
                                    old = last_reset_counter,
                                    new = counter,
                                    "bridge: state reset by BRIDGE_RESET_COUNTER"
                                );
                            }
                            acp_events::dispatch_and_notify(&mut state, &event);
                        }
                    }
                }
            }
        }
    })
}
