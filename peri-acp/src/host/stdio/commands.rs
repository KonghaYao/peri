//! AvailableCommands 通知辅助，供 session/new 和 session/load 复用。

use std::sync::Arc;

use agent_client_protocol::{
    schema::v1::{SessionId, SessionNotification, SessionUpdate},
    Client, ConnectionTo,
};
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::PeriCaps;

/// 发送 AvailableCommandsUpdate 通知（Phase 6 A4：投影数据源 = 注册表
/// `snapshot()`——本地 skills（C1）/ ui 明细 / 插件静态条目（B2）均已由
/// 各自注册路径写入注册表，MCP 条目由发现管线（A3）异步注入）；**注册表
/// on_change 为投影重建重发的唯一触发源**。
///
/// 时序契约（P2-6）：广播入口先摘除旧 on_change，再执行 ui 注册（一次性、
/// 幂等），最后挂新回调。首次广播时旧回调不存在（防双发约束不变）；
/// session/load 对同一 session 重广播时，ui 注册动作不再触发旧回调（发往
/// 旧连接捕获的 transport/cx 快照）。
pub(super) fn send_available_commands(
    session_id: &SessionId,
    cx: &ConnectionTo<Client>,
    caps: &PeriCaps,
    command_registry: Option<Arc<CommandRegistry>>,
) {
    let Some(command_registry) = command_registry else {
        tracing::warn!(
            session_id = %session_id,
            "send_available_commands: 无 session 级命令注册表，跳过广播"
        );
        return;
    };
    // 时序（防双发）：ui 注册（一次性、幂等）必须在 set_on_change 挂载之前
    // 完成；on_change 回调内直接重建 snapshot 投影，不重放 ui 注册（P1-1：
    // 不再每次投影重建刷 11 条冲突 warn）。
    command_registry.set_on_change(None);
    crate::dispatch::commands::register_ui_entries(caps, &command_registry);
    let update = crate::dispatch::commands::build_available_commands_update(
        &command_registry.snapshot(),
        caps,
    );
    tracing::info!(
        target: "acp_stdio.commands",
        "send_available_commands: 注册表 snapshot 投影构建完成"
    );
    let notif = SessionNotification::new(
        session_id.clone(),
        SessionUpdate::AvailableCommandsUpdate(update),
    );
    match cx.send_notification(notif) {
        Ok(()) => tracing::info!(
            target: "acp_stdio.commands",
            "send_available_commands: 通知发送成功"
        ),
        Err(e) => tracing::error!(
            target: "acp_stdio.commands",
            error = %e,
            "send_available_commands: 通知发送失败"
        ),
    }

    // 注册表 on_change → 投影重建重发（唯一触发源）。防引用环：回调只捕获
    // Weak(command_registry) + 不可变快照数据，session 销毁（registry 无强
    // 引用）后 upgrade 失败即静默返回。
    let weak = Arc::downgrade(&command_registry);
    let cx = cx.clone();
    let caps = caps.clone();
    let session_id = session_id.clone();
    command_registry.set_on_change(Some(Arc::new(move || {
        let Some(reg) = weak.upgrade() else {
            return;
        };
        let update =
            crate::dispatch::commands::build_available_commands_update(&reg.snapshot(), &caps);
        let notif = SessionNotification::new(
            session_id.clone(),
            SessionUpdate::AvailableCommandsUpdate(update),
        );
        match cx.send_notification(notif) {
            Ok(()) => tracing::info!(
                target: "acp_stdio.commands",
                "send_available_commands: 回调重发通知成功"
            ),
            Err(e) => tracing::error!(
                target: "acp_stdio.commands",
                error = %e,
                "send_available_commands: 回调重发通知失败"
            ),
        }
    })));
}

#[cfg(test)]
#[path = "commands_test.rs"]
mod tests;
