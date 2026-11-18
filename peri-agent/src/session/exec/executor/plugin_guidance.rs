//! Host-side plugin usage guidance（plan 9.1 / 9.3）。
//!
//! 客户端只提交插件 ID；指引中的 Loader 名称来自当前会话的受控池投影与已加载
//! 插件元数据。slash 插件命令与显式选择共用本模块的同一过程，命令处理器不连接
//! MCP，也不自行拼装指引。

use std::collections::HashSet;

use peri_acp_types::mcp_control::DynamicLoaderBinding;
use peri_acp_types::plugin::LoadedPlugin;
use peri_acp_types::session::MessageKind;
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session::MessageSource;
use peri_acp_types::session::PluginSelection;
use peri_acp_types::session::QueuedMessage;
use peri_acp_types::system_reminder::ReminderAudience;
use peri_acp_types::system_reminder::ReminderAudiences;
use peri_acp_types::system_reminder::ReminderCategory;
use peri_acp_types::system_reminder::ReminderDelivery;
use peri_acp_types::system_reminder::ReminderSeverity;
use peri_acp_types::system_reminder::ReminderSource;
use peri_acp_types::system_reminder::SystemReminder;
use peri_acp_types::system_reminder::TrustedSystemReminderFactory;
use peri_acp_types::system_reminder::MAX_REMINDER_BODY_BYTES;
use peri_acp_types::system_reminder::SYSTEM_REMINDER_VERSION;

/// 合并本轮插件选择：显式选择在前，已移交队列输入居中，slash 来源在后。
///
/// 按 plugin ID 去重并保持首次出现顺序；同一插件不会重复生成指引。
pub(crate) fn merge_plugin_selections(
    explicit: Vec<PluginSelection>,
    handed_off: Vec<PluginSelection>,
    slash_plugin: Option<&str>,
) -> Vec<PluginSelection> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for selection in explicit
        .into_iter()
        .chain(handed_off)
        .chain(slash_plugin.map(PluginSelection::new))
    {
        if seen.insert(selection.plugin_id.clone()) {
            merged.push(selection);
        }
    }
    merged
}

/// 为每个已选且存在可见 Loader 的插件生成一段指引。
///
/// 未安装的选择与没有可见 Loader 的插件不产出文本：不能暗示不存在的业务能力。
pub(crate) fn guidance_texts(
    selections: &[PluginSelection],
    plugins: &[LoadedPlugin],
    loaders: &[DynamicLoaderBinding],
) -> Vec<String> {
    let mut emitted = HashSet::new();
    let mut texts = Vec::new();
    for selection in selections {
        let mut matching: Vec<&LoadedPlugin> = plugins
            .iter()
            .filter(|plugin| plugin_matches(&selection.plugin_id, plugin))
            .collect();
        matching.sort_by(|left, right| left.name.cmp(&right.name));
        for plugin in matching {
            let source = plugin_source_id(plugin);
            if !emitted.insert(source.clone()) {
                continue;
            }
            let mut loader_names: Vec<&str> = loaders
                .iter()
                .filter(|binding| binding.plugin_source.as_deref() == Some(source.as_str()))
                .map(|binding| binding.loader.as_str())
                .collect();
            loader_names.sort_unstable();
            loader_names.dedup();
            if loader_names.is_empty() {
                tracing::debug!(
                    plugin = %plugin.name,
                    "selected plugin has no visible dynamic MCP loader; guidance omitted"
                );
                continue;
            }
            texts.push(guidance_for_plugin(&plugin.name, &loader_names));
        }
    }
    texts
}

fn plugin_matches(selection: &str, plugin: &LoadedPlugin) -> bool {
    selection == plugin_source_id(plugin)
        || selection == plugin.name
        || selection == plugin.scope.plugin_id
}

/// 与 `collect_plugin_mcp_servers` 的来源标识同源（marketplace 为空仍保留分隔符）。
fn plugin_source_id(plugin: &LoadedPlugin) -> String {
    format!("{}@{}", plugin.name, plugin.marketplace)
}

fn guidance_for_plugin(name: &str, loaders: &[&str]) -> String {
    let calls = match loaders {
        [single] => format!("call ToolInvoke with name \"{single}\" and arguments {{}}"),
        many => format!(
            "call ToolInvoke with one of these names and arguments {{}}: {}",
            many.iter()
                .map(|loader| format!("\"{loader}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    format!(
        "The selected plugin is {name}. To use its MCP capabilities, {calls}. \
         After loading succeeds, use ToolSearch in a subsequent model step to find \
         the tools needed for the task."
    )
}

/// 以 Model-only canonical reminder 投递指引；不修改 frozen system prompt。
pub(crate) async fn push_plugin_guidance(queue: &MessageQueue, texts: &[String]) {
    for text in texts {
        let mut remaining = text.as_str();
        while !remaining.is_empty() {
            let mut end = remaining.len().min(MAX_REMINDER_BODY_BYTES);
            while !remaining.is_char_boundary(end) {
                end -= 1;
            }
            let reminder = TrustedSystemReminderFactory::for_producer()
                .construct(SystemReminder {
                    version: SYSTEM_REMINDER_VERSION,
                    category: ReminderCategory::Guidance,
                    source: ReminderSource("plugin_usage".into()),
                    kind: "plugin_guidance".into(),
                    severity: ReminderSeverity::Info,
                    delivery: ReminderDelivery::Required,
                    audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                    body: remaining[..end].to_owned(),
                    summary: None,
                    metadata: serde_json::json!({}),
                })
                .expect("plugin guidance is bounded before reminder construction");
            let message = QueuedMessage::system_reminder(
                MessageKind::Info,
                MessageSource::SystemInjected,
                reminder,
            );
            if let Err(error) = queue.enqueue(message).await {
                tracing::warn!(%error, "plugin guidance enqueue failed");
            }
            remaining = &remaining[end..];
        }
    }
}

#[cfg(test)]
#[path = "plugin_guidance_test.rs"]
mod tests;
