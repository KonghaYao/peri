//! AvailableCommands 通知辅助，供 session/new 和 session/load 复用。

use agent_client_protocol::{
    schema::v1::{AvailableCommandsUpdate, SessionId, SessionNotification, SessionUpdate},
    Client, ConnectionTo,
};
use peri_acp_types::ports::SkillsPort;
use peri_acp_types::skills::SkillRoot;
use peri_acp_types::PeriCaps;

/// 扫描 skill 目录并发送 AvailableCommandsUpdate 通知。
///
/// skills 扫描经注入的 [`SkillsPort`]（宿主装配点构造实现后注入，
/// §0 依赖方向）；ACP 协议面不直调业务 crate。
pub(super) fn send_available_commands(
    skills_port: &dyn SkillsPort,
    cwd: &str,
    plugin_skill_roots: &[SkillRoot],
    session_id: &SessionId,
    cx: &ConnectionTo<Client>,
    caps: &PeriCaps,
) {
    let skills = skills_port.available_skills(cwd, plugin_skill_roots);
    let skill_names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
    tracing::info!(
        target: "acp_stdio.commands",
        skills_count = skills.len(),
        ?skill_names,
        "send_available_commands: scan skill roots 完成"
    );
    let cmds = crate::dispatch::build_available_commands(&skills);
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
