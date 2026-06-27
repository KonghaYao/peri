use std::sync::Arc;

use crate::{app::App, command::Command, i18n::LcRegistry};

pub struct ChannelCommand;

impl Command for ChannelCommand {
    fn name(&self) -> &str {
        "channel"
    }

    fn description(&self, lc: &LcRegistry) -> String {
        lc.tr("command-channel-desc").to_string()
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["ch"]
    }

    fn execute(&self, app: &mut App, args: &str) {
        let lc = &app.services.lc;
        let args = args.trim();

        if args.is_empty() || args == "status" {
            self.show_status(app);
            return;
        }

        if args == "close" {
            self.close_all(app);
            return;
        }

        if let Some(source) = args.strip_prefix("open ") {
            self.open_channel(app, source.trim());
            return;
        }

        if let Some(server_name) = args.strip_prefix("close ") {
            self.close_one(app, server_name.trim());
            return;
        }

        let usage = lc.tr("command-channel-usage").to_string();
        app.push_system_note(usage);
    }
}

impl ChannelCommand {
    fn open_channel(&self, app: &mut App, source: &str) {
        let lc = &app.services.lc;
        let channel_state = match &app.services.channel_state {
            Some(cs) => Arc::clone(cs),
            None => {
                self.add_note(app, &lc.tr("command-channel-not-init"));
                return;
            }
        };

        let server_name = extract_server_name(source);

        // Check if server has channel capability
        let has_capability = app
            .services
            .mcp_pool
            .as_ref()
            .map(|pool| {
                pool.get_client(&server_name)
                    .map(|h| h.channel_capable)
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        if !has_capability {
            self.add_note(
                app,
                &lc.tr_args(
                    "command-channel-unavailable",
                    &[("server".into(), server_name.to_string().into())],
                ),
            );
            return;
        }

        // Authorize the channel
        channel_state.authorize(&server_name, source.to_string());

        // Register message receiver for the active session
        let session_id = app
            .session_mgr
            .current_mut()
            .metadata
            .session_id
            .to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        channel_state.register_session(session_id, tx);
        app.session_mgr
            .current_mut()
            .messages
            .channel_notification_rx = Some(rx);

        self.add_note(
            app,
            &lc.tr_args(
                "command-channel-opened",
                &[("source".into(), source.to_string().into())],
            ),
        );
    }

    fn close_all(&self, app: &mut App) {
        let lc = &app.services.lc;
        if let Some(cs) = &app.services.channel_state {
            cs.close_all();
            app.session_mgr
                .current_mut()
                .messages
                .channel_notification_rx = None;
            self.add_note(app, &lc.tr("command-channel-all-closed"));
        }
    }

    fn close_one(&self, app: &mut App, server_name: &str) {
        let lc = &app.services.lc;
        if let Some(cs) = &app.services.channel_state {
            cs.revoke(server_name);
            self.add_note(
                app,
                &lc.tr_args(
                    "command-channel-closed",
                    &[("server".into(), server_name.to_string().into())],
                ),
            );
        }
    }

    fn show_status(&self, app: &mut App) {
        let lc = &app.services.lc;
        let channel_state = app.services.channel_state.clone();
        let msg = if let Some(cs) = &channel_state {
            let authorized = cs.authorized.read();
            if authorized.is_empty() {
                lc.tr("command-channel-no-channels").to_string()
            } else {
                let mut status = lc.tr("command-channel-list-header").to_string();
                status.push('\n');
                for (server, source) in authorized.iter() {
                    status.push_str(&lc.tr_args(
                        "command-channel-list-item",
                        &[("source".into(), format!("{} → {}", server, source).into())],
                    ));
                    status.push('\n');
                }
                status
            }
        } else {
            lc.tr("command-channel-not-init").to_string()
        };
        self.add_note(app, &msg);
    }

    fn add_note(&self, app: &mut App, msg: &str) {
        // UI 反馈：走 ephemeral SystemNote VM 路径，不污染 BaseMessage[] / Prompt Cache。
        app.push_system_note(msg.to_string());
    }
}

/// 从 channel source 标识符提取 MCP server name（对齐 config 中的命名格式）
///
/// plugin 格式移除 @marketplace 保留 `plugin:{name}:{server}`：
/// - `"plugin:weixin@anthropic:weixin"` → `"plugin:weixin:weixin"`
/// - `"plugin:weixin:weixin"` → `"plugin:weixin:weixin"`
///
/// server 格式直接取出 server name：
/// - `"server:my-mcp"` → `"my-mcp"`
///
/// 此函数与 peri-middlewares/src/mcp/channel_handler.rs 中的 extract_server_name 逻辑完全一致。
fn extract_server_name(source: &str) -> String {
    if let Some(rest) = source.strip_prefix("plugin:") {
        // 移除 @marketplace 部分：从 "@anthropic:server" 中删掉 "@anthropic"
        let cleaned = if let Some(at_pos) = rest.find('@') {
            if let Some(colon_pos) = rest[at_pos..].find(':') {
                format!("{}{}", &rest[..at_pos], &rest[at_pos + colon_pos..])
            } else {
                rest[..at_pos].to_string()
            }
        } else {
            rest.to_string()
        };
        format!("plugin:{}", cleaned)
    } else if let Some(rest) = source.strip_prefix("server:") {
        rest.to_string()
    } else {
        source.to_string()
    }
}
