use super::*;
use crate::app::App;
use crate::command::Command;
use crate::runtime::effect::Effect;

async fn make_headless() -> App {
    let (app, _handle) = App::new_headless(80, 24).await;
    app
}

/// 辅助：从 `Vec<Effect>` 中提取首个 `PushSystemNote` 文本（若有）。
fn first_system_note_text(effects: &[Effect]) -> Option<String> {
    for e in effects {
        if let Effect::ShowNotification(t) = e {
            return Some(t.clone());
        }
    }
    None
}

#[tokio::test]
async fn test_plugin_empty_args_opens_panel() {
    let mut app = make_headless().await;
    let cmd = PluginCommand;
    let effects = cmd.execute(&mut app, "");
    // Phase E: v2 Plugin panel is opened via state machine, returns effects
    assert!(
        effects.iter().any(|e| matches!(e, Effect::OpenPanel(crate::app::PanelKind::Plugin))),
        "无参数应返回 OpenPanel(Plugin) effect"
    );
}

#[tokio::test]
async fn test_plugin_marketplace_add_to_existing_shows_error() {
    let mut app = make_headless().await;
    let cmd = PluginCommand;
    // anthropics/claude-plugins-official 已内置，add 会触发"已存在"错误
    let effects = cmd.execute(&mut app, "marketplace add anthropics/claude-plugins-official");
    let msg = first_system_note_text(&effects);
    assert!(msg.is_some(), "marketplace add（重复）应产生错误消息");
}

#[tokio::test]
async fn test_plugin_marketplace_update_records_note() {
    let mut app = make_headless().await;
    let cmd = PluginCommand;
    // Phase E: marketplace update pushes system note with marketplace name
    let effects = cmd.execute(&mut app, "marketplace update nonexistent-marketplace");
    let msg = first_system_note_text(&effects);
    assert!(msg.is_some(), "marketplace update 应产生系统提示");
    assert!(
        msg.unwrap().contains("nonexistent-marketplace"),
        "提示应包含 marketplace 名称"
    );
}

#[tokio::test]
async fn test_plugin_install_missing_shows_error() {
    let mut app = make_headless().await;
    let cmd = PluginCommand;
    let effects = cmd.execute(&mut app, "install none@none");
    let msg = first_system_note_text(&effects);
    assert!(msg.is_some(), "install（不存在）应产生错误消息");
}

#[tokio::test]
async fn test_plugin_unknown_subcommand_shows_usage() {
    let mut app = make_headless().await;
    let cmd = PluginCommand;
    let effects = cmd.execute(&mut app, "unknown sub command");
    let msg = first_system_note_text(&effects);
    assert!(msg.is_some(), "未知子命令应显示用法提示");
    // 默认 locale 可能是 en/zh-CN，两种文案都接受
    let text = msg.unwrap();
    assert!(
        text.contains("用法") || text.contains("Usage"),
        "未知子命令的消息应包含用法说明，实际: {}",
        text
    );
}
