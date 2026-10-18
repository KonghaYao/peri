//! 终态展示窗口、身份账本与有界交付日志的存储辅助。
//!
//! 生命周期与容量决策见主模块 `MailboxState::terminal_identities` 的注释：
//! 身份账本随 generation 回收；展示窗口与交付日志有界，但交付槽位在 enqueue
//! 时预留，因此日志不会在消息写入 transcript 后溢出。

use peri_acp_types::messages::MessageContent;
use peri_acp_types::session::UserInputState;

use super::MailboxState;

pub(super) const TERMINAL_CAPACITY: usize = 128;
pub(super) const DELIVERY_JOURNAL_CAPACITY: usize = 128;

/// 已进入 canonical transcript、等待事件泵按序推送的交付确认。
#[derive(Clone, Debug)]
pub(crate) struct PendingDeliveryNotification {
    pub(crate) input_id: String,
    pub(crate) content: MessageContent,
}

pub(super) struct TerminalIdentity {
    pub(super) fingerprint: u64,
    pub(super) state: UserInputState,
}

pub(super) struct TerminalDisplay {
    pub(super) input_id: String,
    pub(super) published: bool,
}

/// enqueue 时已预留、尚未转成日志条目的槽位。
pub(super) fn reserved_delivery_slots(state: &MailboxState) -> usize {
    state
        .records
        .iter()
        .filter(|record| record.delivery_reserved)
        .count()
}

pub(super) fn retain_terminal(state: &mut MailboxState, id: &str) {
    let Ok(input_uuid) = uuid::Uuid::parse_str(id) else {
        return;
    };
    let record = state
        .records
        .iter()
        .find(|record| record.input.input_id == id)
        .expect("终态记录存在");
    if state.terminal_identities.contains_key(&input_uuid) {
        return;
    }
    state.terminal_identities.insert(
        input_uuid,
        TerminalIdentity {
            fingerprint: record.fingerprint,
            state: record.state,
        },
    );
    state.terminal_display.push(TerminalDisplay {
        input_id: id.to_owned(),
        published: false,
    });
}

// 仅淘汰按终结顺序排列且已发布的展示记录；不删除 generation 内身份账本。
// 超出恢复窗口且所有事件均丢失时，客户端必须保持未知，不能猜测交付位置。
pub(super) fn prune_terminal_records(state: &mut MailboxState) {
    while state.terminal_display.len() > TERMINAL_CAPACITY {
        if !state.terminal_display[0].published {
            break;
        }
        let removed = state.terminal_display.remove(0);
        state
            .records
            .retain(|record| record.input.input_id != removed.input_id);
        state.delivered_notifications.remove(&removed.input_id);
    }
}
