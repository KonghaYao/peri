//! 发送 config_options 更新通知，供 set_mode / set_model / set_config_option / update_config 复用。

use crate::dispatch;
use agent_client_protocol::{
    schema::v1::{
        ConfigOptionUpdate, SessionConfigOption, SessionId, SessionNotification, SessionUpdate,
    },
    Client, ConnectionTo,
};

use super::context::StdioContext;

/// 构建并发送 ConfigOptionUpdate 通知。
///
/// 返回 config_options 列表供响应体使用（set_config_option 需附加到响应中）。
pub(super) fn send_config_update(
    ctx: &StdioContext,
    session_id: &SessionId,
    cx: &ConnectionTo<Client>,
) -> Vec<SessionConfigOption> {
    let c = ctx.peri_config.read();
    let p = ctx.provider.read();
    let options = dispatch::config_update::make_config_options(&c, &p, ctx.permission_mode.load());
    let notif = SessionNotification::new(
        session_id.clone(),
        SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(options.clone())),
    );
    let _ = cx.send_notification(notif);
    options
}
