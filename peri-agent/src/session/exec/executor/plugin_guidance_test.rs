//! 插件指引生成与投递（plan 9.1 / 9.3）的契约测试。

use std::collections::HashMap;
use std::path::PathBuf;

use peri_acp_types::mcp_control::DynamicLoaderBinding;
use peri_acp_types::plugin::InstallScope;
use peri_acp_types::plugin::LoadedPlugin;
use peri_acp_types::plugin::PluginOrigin;
use peri_acp_types::plugin::PluginScope;
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session::PluginSelection;
use peri_acp_types::session::QueuedPayload;
use peri_acp_types::system_reminder::ReminderAudience;
use peri_acp_types::system_reminder::ReminderCategory;
use peri_acp_types::system_reminder::MAX_REMINDER_BODY_BYTES;

use crate::session::exec::executor::plugin_guidance::guidance_texts;
use crate::session::exec::executor::plugin_guidance::merge_plugin_selections;
use crate::session::exec::executor::plugin_guidance::push_plugin_guidance;

const RESEARCH_LOADER: &str = "LoadMCP__research_a1b2c3d4";

fn loaded_plugin(name: &str, marketplace: &str) -> LoadedPlugin {
    LoadedPlugin {
        name: name.into(),
        version: "1.0.0".into(),
        install_path: PathBuf::new(),
        manifest: serde_json::from_value(serde_json::json!({ "name": name }))
            .expect("minimal plugin manifest"),
        commands: vec![],
        skills_roots: vec![],
        agents_dirs: vec![],
        mcp_servers: HashMap::new(),
        data_path: PathBuf::new(),
        hooks_config: None,
        marketplace: marketplace.into(),
        scope: PluginScope {
            plugin_id: format!("{name}@{marketplace}"),
            origin: PluginOrigin::PeriInstalled,
            install_scope: InstallScope::User,
            project_path: None,
        },
    }
}

fn loader(loader: &str, plugin: &str, marketplace: &str) -> DynamicLoaderBinding {
    DynamicLoaderBinding {
        loader: loader.into(),
        server_key: format!("plugin:{plugin}:search"),
        plugin_source: Some(format!("{plugin}@{marketplace}")),
    }
}

#[test]
fn test_guidance_uses_selected_plugin_and_bound_loader() {
    let texts = guidance_texts(
        &[PluginSelection::new("research@market")],
        &[loaded_plugin("research", "market")],
        &[loader(RESEARCH_LOADER, "research", "market")],
    );

    assert_eq!(
        texts,
        vec![format!(
            "The selected plugin is research. To use its MCP capabilities, call ToolInvoke \
             with name \"{RESEARCH_LOADER}\" and arguments {{}}. After loading succeeds, \
             use ToolSearch in a subsequent model step to find the tools needed for the task."
        )],
        "指引必须点名所选插件与受控 Loader 名称"
    );
}

/// slash 来源的插件名与显式选择共用同一生成过程：同一插件得到相同指引。
#[test]
fn test_slash_plugin_name_reuses_selection_guidance() {
    let plugins = [loaded_plugin("research", "market")];
    let loaders = [loader(RESEARCH_LOADER, "research", "market")];

    let selected = guidance_texts(
        &[PluginSelection::new("research@market")],
        &plugins,
        &loaders,
    );
    let slash = guidance_texts(&[PluginSelection::new("research")], &plugins, &loaders);

    assert_eq!(slash, selected, "slash 不能产生第二套指引文本");
    assert_eq!(slash.len(), 1);
}

#[test]
fn test_guidance_resolves_plugin_scope_before_loader_binding() {
    let texts = guidance_texts(
        &[PluginSelection::new("research@alpha")],
        &[
            loaded_plugin("research", "alpha"),
            loaded_plugin("research", "beta"),
        ],
        &[
            loader("LoadMCP__research_alpha", "research", "alpha"),
            loader("LoadMCP__research_beta", "research", "beta"),
        ],
    );

    assert_eq!(texts.len(), 1, "同名插件必须按来源身份唯一定位");
    let text = &texts[0];
    assert!(text.contains("LoadMCP__research_alpha"), "{text}");
    assert!(
        !text.contains("LoadMCP__research_beta"),
        "未选择来源的 Loader 不得进入指引: {text}"
    );
}

#[test]
fn test_guidance_omits_uninstalled_or_unbound_plugins() {
    let texts = guidance_texts(
        &[
            PluginSelection::new("missing"),
            PluginSelection::new("research@market"),
        ],
        &[loaded_plugin("research", "market")],
        &[],
    );
    assert!(texts.is_empty(), "没有可见 Loader 时不能暗示业务能力可用");
}

#[test]
fn test_guidance_lists_multiple_loaders_in_stable_order() {
    let texts = guidance_texts(
        &[PluginSelection::new("research")],
        &[loaded_plugin("research", "market")],
        &[
            loader("LoadMCP__research_b", "research", "market"),
            loader("LoadMCP__research_a", "research", "market"),
            loader("LoadMCP__research_a", "research", "market"),
        ],
    );

    assert_eq!(texts.len(), 1);
    let text = &texts[0];
    let a = text.find("LoadMCP__research_a").expect("first loader");
    let b = text.find("LoadMCP__research_b").expect("second loader");
    assert!(a < b, "Loader 名称必须稳定排序: {text}");
    assert_eq!(
        text.matches("LoadMCP__research_a").count(),
        1,
        "重复绑定不得重复列出: {text}"
    );
}

#[test]
fn test_merge_keeps_explicit_then_handed_off_then_slash_without_duplicates() {
    let merged = merge_plugin_selections(
        vec![PluginSelection::new("alpha")],
        vec![
            PluginSelection::new("beta@market"),
            PluginSelection::new("alpha"),
        ],
        Some("research"),
    );

    assert_eq!(
        merged
            .iter()
            .map(|selection| selection.plugin_id.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta@market", "research"]
    );
}

#[test]
fn test_guidance_is_delivered_as_model_only_canonical_reminder() {
    let queue = MessageQueue::new();
    let body = "x".repeat(MAX_REMINDER_BODY_BYTES + 10);
    futures::executor::block_on(push_plugin_guidance(&queue, std::slice::from_ref(&body)))
        .expect("open queue accepts guidance");

    assert!(!queue.has_wake_up_sync(), "指引不单独唤醒新执行");
    let reminders: Vec<_> = queue
        .drain_all_sync()
        .into_iter()
        .map(|message| {
            let QueuedPayload::SystemReminder(reminder) = message.payload else {
                panic!("插件指引必须是受控 Reminder");
            };
            assert_eq!(reminder.as_reminder().category, ReminderCategory::Guidance);
            assert_eq!(
                reminder.as_reminder().audiences.0,
                [ReminderAudience::Model],
                "指引只进入模型请求，不额外展示在聊天区"
            );
            reminder.as_reminder().body.clone()
        })
        .collect();

    assert!(reminders.len() > 1, "超限内容应按 UTF-8 边界安全分段");
    assert_eq!(reminders.concat(), body, "分段不能丢字");
}

/// 入队失败必须返回给调用方：不能只记录日志后把部分投递当作成功。
#[test]
fn test_guidance_enqueue_failure_is_returned() {
    let queue = MessageQueue::new();
    queue.close_admission();
    let error = futures::executor::block_on(push_plugin_guidance(
        &queue,
        &["selected plugin guidance".to_string()],
    ))
    .expect_err("关闭入队闸门后指引投递必须失败");
    assert!(
        error.to_string().contains("closed to new enqueues"),
        "必须保留队列关闭原因: {error}"
    );
    assert!(queue.is_empty_sync(), "失败时不得写入部分指引");
}
