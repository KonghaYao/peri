//! AvailableCommands 通知辅助，供 session/new 和 session/load 复用。

use std::sync::Arc;

use agent_client_protocol::{
    schema::v1::{SessionId, SessionNotification, SessionUpdate},
    Client, ConnectionTo,
};
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::ports::SkillsPort;
use peri_acp_types::skills::SkillRoot;
use peri_acp_types::PeriCaps;

/// 扫描 skill 目录（本地 + MCP 合并）并发送 AvailableCommandsUpdate 通知；
/// registry Some 时注册 on_change 回调（发现完成/条目变化时同步重发，DD-5）。
///
/// skills 扫描经注入的 [`SkillsPort`]（宿主装配点构造实现后注入，
/// §0 依赖方向）；ACP 协议面不直调业务 crate。
pub(super) fn send_available_commands(
    skills_port: &Arc<dyn SkillsPort>,
    cwd: &str,
    plugin_skill_roots: &[SkillRoot],
    session_id: &SessionId,
    cx: &ConnectionTo<Client>,
    caps: &PeriCaps,
    registry: Option<Arc<McpSkillRegistry>>,
) {
    let skills = skills_port.available_skills(cwd, plugin_skill_roots);
    let skill_names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
    tracing::info!(
        target: "acp_stdio.commands",
        skills_count = skills.len(),
        ?skill_names,
        "send_available_commands: scan skill roots 完成"
    );
    let mcp = registry
        .as_ref()
        .map(|reg| reg.all_skills())
        .unwrap_or_default();
    let update = crate::dispatch::commands::build_available_commands_update(&skills, &mcp, caps);
    tracing::info!(
        target: "acp_stdio.commands",
        local_count = skills.len(),
        mcp_count = mcp.len(),
        "send_available_commands: build_available_commands_update 完成"
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

    // 发现完成/条目变化 → 重发（DD-5）。防引用环：回调只捕获 Weak，session
    // 销毁（registry 无强引用）后 upgrade 失败即静默返回。
    if let Some(reg) = &registry {
        let weak = Arc::downgrade(reg);
        let cx = cx.clone();
        let skills_port = Arc::clone(skills_port);
        let cwd = cwd.to_string();
        let plugin_skill_roots = plugin_skill_roots.to_vec();
        let caps = caps.clone();
        let session_id = session_id.clone();
        reg.set_on_change(Some(Arc::new(move || {
            let Some(reg) = weak.upgrade() else {
                return;
            };
            let skills = skills_port.available_skills(&cwd, &plugin_skill_roots);
            let mcp = reg.all_skills();
            let update =
                crate::dispatch::commands::build_available_commands_update(&skills, &mcp, &caps);
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
}
