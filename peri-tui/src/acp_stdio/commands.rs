//! AvailableCommands 通知辅助，供 session/new 和 session/load 复用。

use agent_client_protocol::{
    Client, ConnectionTo,
    schema::v1::{AvailableCommandsUpdate, SessionId, SessionNotification, SessionUpdate},
};
use peri_acp_types::PeriCaps;

/// 扫描 skill 目录并发送 AvailableCommandsUpdate 通知。
pub(super) fn send_available_commands(
    cwd: &str,
    plugin_skill_roots: &[peri_middlewares::skills::SkillRoot],
    session_id: &SessionId,
    cx: &ConnectionTo<Client>,
    caps: &PeriCaps,
) {
    let disable_bundled = peri_middlewares::skills::load_disable_bundled_skills();
    let skill_roots = peri_middlewares::SkillsMiddleware::resolve_roots_static(
        cwd,
        plugin_skill_roots.to_vec(),
        disable_bundled, // Stdio 侧仅用于显示
    );
    let skills = peri_middlewares::skills::scan_skill_roots(&skill_roots);
    let cmds = peri_acp::dispatch::build_available_commands(&skills);
    let update = if caps.skill_names {
        let meta = skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
        AvailableCommandsUpdate::new(cmds).meta(
            serde_json::json!({"skillNames": meta})
                .as_object()
                .unwrap()
                .clone(),
        )
    } else {
        AvailableCommandsUpdate::new(cmds)
    };
    let notif = SessionNotification::new(
        session_id.clone(),
        SessionUpdate::AvailableCommandsUpdate(update),
    );
    let _ = cx.send_notification(notif);
}
