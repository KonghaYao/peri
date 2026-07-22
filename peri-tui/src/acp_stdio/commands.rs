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
    tracing::info!(
        target: "acp_stdio.commands",
        disable_bundled,
        plugin_roots_count = plugin_skill_roots.len(),
        "send_available_commands: 开始扫描 skills"
    );
    let skill_roots = peri_middlewares::SkillsMiddleware::resolve_roots_static(
        cwd,
        plugin_skill_roots.to_vec(),
        disable_bundled, // Stdio 侧仅用于显示
    );
    tracing::info!(
        target: "acp_stdio.commands",
        total_roots = skill_roots.len(),
        "send_available_commands: resolve_roots_static 完成"
    );
    let skills = peri_middlewares::skills::scan_skill_roots(&skill_roots);
    let skill_names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
    tracing::info!(
        target: "acp_stdio.commands",
        skills_count = skills.len(),
        ?skill_names,
        "send_available_commands: scan_skill_roots 完成"
    );
    let cmds = peri_acp::dispatch::build_available_commands(&skills);
    tracing::info!(
        target: "acp_stdio.commands",
        commands_count = cmds.len(),
        "send_available_commands: build_available_commands 完成"
    );
    let update = if caps.skill_names {
        let meta = skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
        tracing::info!(
            target: "acp_stdio.commands",
            caps_skill_names = true,
            ?meta,
            "send_available_commands: 附加 _meta.skillNames"
        );
        AvailableCommandsUpdate::new(cmds).meta(
            serde_json::json!({"skillNames": meta})
                .as_object()
                .unwrap()
                .clone(),
        )
    } else {
        tracing::info!(
            target: "acp_stdio.commands",
            caps_skill_names = false,
            "send_available_commands: caps.skill_names=false，不附加 _meta"
        );
        AvailableCommandsUpdate::new(cmds)
    };
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
}
