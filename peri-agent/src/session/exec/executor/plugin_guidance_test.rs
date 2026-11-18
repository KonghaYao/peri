//! 插件指引生成与投递（plan 9.1 / 9.3）的契约测试。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use peri_acp_types::identity::AttemptId;
use peri_acp_types::mcp_control::DynamicLoaderBinding;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::plugin::InstallScope;
use peri_acp_types::plugin::LoadedPlugin;
use peri_acp_types::plugin::PluginOrigin;
use peri_acp_types::plugin::PluginScope;
use peri_acp_types::session::ExecutionBinding;
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session::MessageSource;
use peri_acp_types::session::MqFilter;
use peri_acp_types::session::MqReceiveDisposition;
use peri_acp_types::session::MqReceivePurpose;
use peri_acp_types::session::MqReceiveStatus;
use peri_acp_types::session::PluginSelection;
use peri_acp_types::session::QueuedMessage;
use peri_acp_types::session::QueuedPayload;
use peri_acp_types::session::TurnId;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::ReminderAudience;
use peri_acp_types::system_reminder::ReminderCategory;
use peri_acp_types::system_reminder::MAX_REMINDER_BODY_BYTES;

use crate::session::exec::executor::plugin_guidance::guidance_texts;
use crate::session::exec::executor::plugin_guidance::merge_plugin_selections;
use crate::session::exec::executor::plugin_guidance::push_plugin_guidance;
use crate::session::exec::executor::plugin_guidance::retain_effective_loaders;
use crate::session::test_resources::mock::MockSessionResources;

const RESEARCH_LOADER: &str = "LoadMCP__research_a1b2c3d4";

struct CatalogTool {
    name: String,
    server: Option<String>,
}

#[async_trait::async_trait]
impl crate::tools::BaseTool for CatalogTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        ""
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn mcp_server_name(&self) -> Option<&str> {
        self.server.as_deref()
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}

fn catalog_tool(name: &str, server: &str) -> Arc<dyn crate::tools::BaseTool> {
    Arc::new(CatalogTool {
        name: name.into(),
        server: Some(server.into()),
    })
}

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

fn execution() -> ExecutionBinding {
    ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
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
fn test_guidance_omits_loaders_outside_the_effective_reason_catalog() {
    let binding = loader(RESEARCH_LOADER, "research", "market");
    let matching = [catalog_tool(RESEARCH_LOADER, &binding.server_key)];

    assert_eq!(
        retain_effective_loaders(vec![binding.clone()], &matching, &|_| true),
        vec![binding.clone()],
        "有效目录中的 Loader 保留"
    );
    assert!(
        retain_effective_loaders(vec![binding.clone()], &matching, &|_| false).is_empty(),
        "会话工具策略拒绝的 Loader 不得进入指引"
    );
    assert!(
        retain_effective_loaders(
            vec![binding.clone()],
            &[catalog_tool(RESEARCH_LOADER, "plugin:other:search")],
            &|_| true,
        )
        .is_empty(),
        "同名但来源不同的工具不能冒充 Loader"
    );
    assert!(
        retain_effective_loaders(vec![binding], &[], &|_| true).is_empty(),
        "目录不可用时不得提示调用"
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
    futures::executor::block_on(push_plugin_guidance(
        &queue,
        std::slice::from_ref(&body),
        &execution(),
    ))
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
        &execution(),
    ))
    .expect_err("关闭入队闸门后指引投递必须失败");
    assert!(
        error.to_string().contains("closed to new enqueues"),
        "必须保留队列关闭原因: {error}"
    );
    assert!(error.cleanup.is_empty(), "本次失败没有已提交前缀");
    assert!(
        error.uncertain.is_none(),
        "准入关闭发生在写入前，不得伪报结果未知"
    );
    assert!(queue.is_empty_sync(), "失败时不得留下部分指引");
}

/// guidance 中途失败后，失败 attempt 已提交的 SystemInjected 行必须能清理；
/// 同一 MQ 中的用户输入行不受影响，用户取消契约也不因此放宽。
#[test]
fn test_failed_guidance_cleanup_removes_only_attempt_rows() {
    let queue = MessageQueue::new();
    let prompt = futures::executor::block_on(queue.enqueue_sent(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("run the selected plugin"),
    )))
    .expect("用户输入必须能进入 MQ")
    .entry;
    let body = "x".repeat(MAX_REMINDER_BODY_BYTES + 10);
    let execution = execution();
    let enqueued = futures::executor::block_on(push_plugin_guidance(
        &queue,
        std::slice::from_ref(&body),
        &execution,
    ))
    .expect("开放队列接受指引");
    assert!(enqueued.len() > 1, "指引必须分段以覆盖部分提交前缀");

    let outcome = futures::executor::block_on(
        queue.discard_system_injected_for_attempt(&enqueued, &execution),
    )
    .expect("失败 attempt 的 SystemInjected 行必须可清理");
    assert_eq!(outcome.items.len(), enqueued.len());
    assert!(
        outcome
            .items
            .iter()
            .all(|item| item.status == MqReceiveStatus::Suppressed),
        "清理只删除 MQ 行，不写历史: {:?}",
        outcome.items
    );

    let remaining = futures::executor::block_on(queue.snapshot()).expect("MQ snapshot");
    assert_eq!(remaining.len(), 1, "只应保留用户输入行: {remaining:?}");
    assert_eq!(remaining[0].id(), prompt.id);
    assert_eq!(remaining[0].source, MessageSource::UserInput);

    let rejection = futures::executor::block_on(
        queue.discard_system_injected_for_attempt(std::slice::from_ref(&prompt), &execution),
    );
    assert!(rejection.is_err(), "内部清理不得删除用户输入行");
    assert_eq!(
        futures::executor::block_on(queue.snapshot())
            .expect("MQ snapshot")
            .len(),
        1,
        "拒绝清理后用户输入行必须仍在 MQ"
    );
}

/// enqueue 返回 PersistenceUncertain 且行已写入：失败 chunk 不在已确认前缀里，
/// 也必须按固定投递身份查回并纳入同一 attempt 清理。
#[test]
fn test_uncertain_guidance_enqueue_removes_written_failed_chunk() {
    let resources = MockSessionResources::new();
    let queue = MessageQueue::bind(resources.clone(), "guidance-uncertain-written".into());
    let prompt = futures::executor::block_on(queue.enqueue_sent(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("run the selected plugin"),
    )))
    .expect("用户输入必须能进入 MQ")
    .entry;
    // 第一段正常写入，第二段写入后返回 PersistenceUncertain。
    resources.fail_mq_enqueue_uncertain_after_write(1);
    let body = "x".repeat(MAX_REMINDER_BODY_BYTES + 10);
    let origin = execution();
    let failure = futures::executor::block_on(push_plugin_guidance(
        &queue,
        std::slice::from_ref(&body),
        &origin,
    ))
    .expect_err("写入结果未知必须让入队失败");
    let uncertain = failure.uncertain.expect("失败片段身份必须保留");
    assert_eq!(
        failure.cleanup.len(),
        2,
        "已确认前缀与结果未知但已查回的片段都必须进入清理集合"
    );
    assert!(
        failure
            .cleanup
            .iter()
            .all(|row| row.source == MessageSource::SystemInjected),
        "清理集合只允许 SystemInjected 行: {:?}",
        failure.cleanup
    );
    assert_eq!(
        failure.cleanup.last().map(|row| row.id),
        Some(uncertain),
        "失败片段必须按入队前固定的身份进入清理集合"
    );

    let outcome = futures::executor::block_on(
        queue.discard_system_injected_for_attempt(&failure.cleanup, &origin),
    )
    .expect("失败 attempt 的 SystemInjected 行必须可清理");
    assert!(
        outcome
            .items
            .iter()
            .all(|item| item.status == MqReceiveStatus::Suppressed),
        "清理只删除 MQ 行，不写历史: {:?}",
        outcome.items
    );

    let remaining = futures::executor::block_on(queue.snapshot()).expect("MQ snapshot");
    assert_eq!(remaining.len(), 1, "只应保留用户输入行: {remaining:?}");
    assert_eq!(remaining[0].id(), prompt.id);
    assert_eq!(remaining[0].source, MessageSource::UserInput);

    // 幂等：同一清理集合再次提交只报 Missing，不产生新的写入或错误。
    let again = futures::executor::block_on(
        queue.discard_system_injected_for_attempt(&failure.cleanup, &origin),
    )
    .expect("重复清理必须幂等");
    assert!(
        again
            .items
            .iter()
            .all(|item| item.status == MqReceiveStatus::Missing),
        "已删除的行只能报 Missing: {:?}",
        again.items
    );
    assert_eq!(
        futures::executor::block_on(queue.snapshot())
            .expect("MQ snapshot")
            .len(),
        1,
        "重复清理不得影响用户输入行"
    );
}

/// enqueue 返回 PersistenceUncertain 但行未写入：只清理已确认前缀，失败身份仍被记录，
/// 重复清理幂等且不制造新行。
#[test]
fn test_uncertain_guidance_enqueue_without_write_is_idempotent() {
    let resources = MockSessionResources::new();
    let queue = MessageQueue::bind(resources.clone(), "guidance-uncertain-missing".into());
    let prompt = futures::executor::block_on(queue.enqueue_sent(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("run the selected plugin"),
    )))
    .expect("用户输入必须能进入 MQ")
    .entry;
    // 第一段正常写入，第二段不写入并返回 PersistenceUncertain。
    resources.fail_mq_enqueue_uncertain_without_write(1);
    let body = "x".repeat(MAX_REMINDER_BODY_BYTES + 10);
    let origin = execution();
    let failure = futures::executor::block_on(push_plugin_guidance(
        &queue,
        std::slice::from_ref(&body),
        &origin,
    ))
    .expect_err("写入结果未知必须让入队失败");
    let uncertain = failure.uncertain.expect("失败片段身份必须保留");
    assert_eq!(failure.cleanup.len(), 1, "只有已确认前缀可以进入清理集合");
    let remaining =
        futures::executor::block_on(queue.query(MqFilter::default())).expect("MQ query");
    assert!(
        remaining.iter().all(|row| row.id != uncertain),
        "未写入的片段不得凭空出现在 MQ: {remaining:?}"
    );

    let outcome = futures::executor::block_on(
        queue.discard_system_injected_for_attempt(&failure.cleanup, &origin),
    )
    .expect("已确认前缀必须可清理");
    assert!(
        outcome
            .items
            .iter()
            .all(|item| item.status == MqReceiveStatus::Suppressed),
        "清理只删除 MQ 行，不写历史: {:?}",
        outcome.items
    );
    let again = futures::executor::block_on(
        queue.discard_system_injected_for_attempt(&failure.cleanup, &origin),
    )
    .expect("重复清理必须幂等");
    assert!(
        again
            .items
            .iter()
            .all(|item| item.status == MqReceiveStatus::Missing),
        "已删除的行只能报 Missing: {:?}",
        again.items
    );

    let remaining = futures::executor::block_on(queue.snapshot()).expect("MQ snapshot");
    assert_eq!(remaining.len(), 1, "只应保留用户输入行: {remaining:?}");
    assert_eq!(remaining[0].id(), prompt.id);
    assert_eq!(remaining[0].source, MessageSource::UserInput);
}

/// store 提交失败 chunk 后清理查询立即失败：行仍带 origin execution，后续执行
/// Receive 必须把它按过期行 Suppress，不能写进新 transcript。
#[test]
fn test_unresolved_written_guidance_is_suppressed_by_later_execution() {
    let resources = MockSessionResources::new();
    let queue = MessageQueue::bind(resources.clone(), "guidance-foreign-execution".into());
    // 未发送的用户输入留在待发送区：Receive 只验证遗留 guidance，不替用户提交输入。
    let prompt = futures::executor::block_on(queue.enqueue(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("run the selected plugin"),
    )))
    .expect("用户输入必须能进入 MQ")
    .entry;
    // 第一段正常写入；第二段写入后返回 PersistenceUncertain，且失败查回立即失败。
    resources.fail_mq_enqueue_uncertain_after_write(1);
    resources.fail_next_mq_queries(1);
    let body = "x".repeat(MAX_REMINDER_BODY_BYTES + 10);
    let origin = execution();
    let failure = futures::executor::block_on(push_plugin_guidance(
        &queue,
        std::slice::from_ref(&body),
        &origin,
    ))
    .expect_err("写入结果未知必须让入队失败");
    let uncertain = failure.uncertain.expect("失败片段身份必须保留");
    assert_eq!(failure.cleanup.len(), 1, "查询失败时只能清理已确认前缀");

    // 快速路径删掉已确认前缀；未查回的 chunk 仍由 store 提交。
    futures::executor::block_on(
        queue.discard_system_injected_for_attempt(&failure.cleanup, &origin),
    )
    .expect("已确认前缀必须可清理");
    let rows = futures::executor::block_on(queue.query(MqFilter::default())).expect("MQ query");
    assert!(
        rows.iter().any(|row| row.id == uncertain),
        "失败 chunk 已由 store 提交: {rows:?}"
    );

    // 后续执行只应 Suppress 遗留行：删行、不写历史，也不影响用户输入。
    let later = execution();
    let owner = queue.owner();
    let permit = owner.begin_receive(MqReceivePurpose::ModelLoop, later);
    let selected =
        futures::executor::block_on(owner.select_for_receive(&permit)).expect("Receive selection");
    assert_eq!(selected.len(), 1, "只选中遗留的 guidance 行: {selected:?}");
    assert_eq!(selected[0].entry.id, uncertain);
    assert_eq!(
        selected[0].disposition,
        MqReceiveDisposition::Suppress,
        "其他执行不得提交失败 attempt 的 guidance"
    );
    let outcome =
        futures::executor::block_on(owner.receive(permit, selected)).expect("Receive outcome");
    assert_eq!(outcome.items[0].status, MqReceiveStatus::Suppressed);

    let remaining = futures::executor::block_on(queue.snapshot()).expect("MQ snapshot");
    assert_eq!(remaining.len(), 1, "只应保留用户输入行: {remaining:?}");
    assert_eq!(remaining[0].id(), prompt.id);
    assert_eq!(remaining[0].source, MessageSource::UserInput);
}

/// 绑定到当前执行的 guidance 仍能被同一执行正常消费。
#[test]
fn test_guidance_is_consumed_by_originating_execution() {
    let queue = MessageQueue::new();
    let body = "selected plugin guidance".to_string();
    let origin = execution();
    futures::executor::block_on(push_plugin_guidance(
        &queue,
        std::slice::from_ref(&body),
        &origin,
    ))
    .expect("open queue accepts guidance");

    let owner = queue.owner();
    let permit = owner.begin_receive(MqReceivePurpose::ModelLoop, origin);
    let selected =
        futures::executor::block_on(owner.select_for_receive(&permit)).expect("Receive selection");
    assert_eq!(selected.len(), 1);
    assert_eq!(
        selected[0].disposition,
        MqReceiveDisposition::Commit,
        "同一执行必须正常提交自己的指引"
    );
    let outcome =
        futures::executor::block_on(owner.receive(permit, selected)).expect("Receive outcome");
    assert_eq!(outcome.items[0].status, MqReceiveStatus::Committed);

    let entry = outcome.items[0].entry.as_ref().expect("committed entry");
    let PersistedPayload::SystemReminder { reminder, .. } = &entry.payload else {
        panic!("插件指引必须是受控 Reminder");
    };
    assert_eq!(reminder.as_reminder().category, ReminderCategory::Guidance);
    assert_eq!(reminder.as_reminder().body, body);
    assert!(queue.is_empty_sync(), "同一执行消费后不应留下 MQ 行");
}
