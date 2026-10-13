use super::*;
use crate::session::transcript::MessageTranscript;
use crate::tools::InvocationOrigin;

fn test_stage() -> StageContext {
    let turn = crate::session::turn::TurnContext::new(
        std::sync::Arc::from("/tmp/project"),
        std::sync::Arc::new(CancellationToken::new()),
    );
    let transcript = std::sync::Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    transcript.write().append(BaseMessage::human("history"));
    StageContext::new(turn, transcript, MessageQueue::new())
}

fn model_batch_context(stage: &StageContext) -> InvocationContext {
    InvocationContext::from_stage(
        stage,
        ExecutionEntryOrigin::ModelBatch,
        stage.session.turn.cancel_token.as_ref().clone(),
    )
}

/// idle（无活跃 turn）执行入口的输入：全部来自真实 session 级事实。
///
/// 生产路径由 session-owned canonical 入口逐项提供（它没有 `StageContext`）；
/// 测试用同一个 `StageContext` 夹具取这些事实，但**不借它伪造 turn**。
fn idle_source(stage: &StageContext, origin: InvocationOrigin) -> SessionInvocationSource {
    SessionInvocationSource {
        session_id: stage.project_session_id(),
        run_id: None,
        agent_id: stage.session.agent_id,
        cwd: std::sync::Arc::clone(&stage.session.turn.cwd),
        messages: Some(std::sync::Arc::new(stage.visible_messages())),
        context_usage: None,
        session_resources: None,
        resolver: std::sync::Arc::clone(&stage.runtime.tool_invocation_resolver),
        chain: std::sync::Arc::clone(&stage.runtime.middleware_chain),
        queue: stage.session.queue.clone(),
        origin,
        cancel: stage.session.turn.cancel_token.as_ref().clone(),
        post_hook_skips: None,
    }
}

/// idle 入口形状：App 来源（已附着附件）。
fn idle_context(stage: &StageContext) -> InvocationContext {
    InvocationContext::from_source(idle_source(
        stage,
        InvocationOrigin::App {
            app_id: "app-idle".into(),
            attachment_id: Some(peri_acp_types::interaction::AttachmentId::new("att-idle")),
            connection_epoch: Some(peri_acp_types::interaction::ConnectionEpoch::initial()),
            server_generation: 1,
        },
    ))
}

#[test]
fn model_batch_specializes_origin_per_call_and_keeps_session_facts() {
    let stage = test_stage();
    stage
        .session
        .session_context
        .write()
        .insert("session_id".into(), "session-7".into());
    let context = model_batch_context(&stage);

    assert_eq!(
        context.origin_for(&ToolCallId::new("call-1")),
        InvocationOrigin::Model {
            tool_call_id: "call-1".into()
        }
    );
    assert_eq!(
        context.origin_for(&ToolCallId::new("call-2")),
        InvocationOrigin::Model {
            tool_call_id: "call-2".into()
        }
    );
    assert_eq!(context.session_id(), Some("session-7"));
    assert_eq!(context.cwd(), "/tmp/project");
    assert_eq!(context.messages().len(), 1, "快照不含本批 AI 消息");
    let turn = context.require_turn("test").expect("active turn");
    assert_eq!(turn.step(), 0);
    assert_eq!(turn.turn_id(), stage.turn_id());
}

/// [回归] 会话身份**不做 agent_id 回退**：没有真实 session 事实时是「无归属」，
/// 不是「用执行实例身份充当会话身份」。
///
/// 假身份曾让 `TrustedInteractionOwner::Model { session_id: <agent_id> }` 成为
/// 「可信事实」，审批的 fail-closed 守卫因此成为死分支（设计 §4.3）。
#[test]
fn missing_session_identity_stays_absent_and_ports_share_the_session_queue() {
    let stage = test_stage();
    let queue = stage.session.queue.clone();
    let context = model_batch_context(&stage);

    assert_eq!(
        context.session_id(),
        None,
        "会话身份缺失必须是 None，不得回退成 agent_id（{}）",
        stage.session.agent_id
    );
    // 有真实会话身份时如实投影（同一投影规则的另一半）。
    stage
        .session
        .session_context
        .write()
        .insert("session_id".into(), "session-9".into());
    assert_eq!(model_batch_context(&stage).session_id(), Some("session-9"));

    let message = QueuedMessage::info(
        crate::session::MessageSource::SystemInjected,
        BaseMessage::human("notice"),
    );
    context.queue().enqueue(message);
    assert_eq!(queue.drain_all().len(), 1, "队列端口必须绑定会话级队列实例");
}

#[test]
fn single_origin_overrides_entry_origin_for_same_session_facts() {
    let stage = test_stage();
    stage
        .session
        .session_context
        .write()
        .insert("session_id".into(), "session-7".into());
    let context = model_batch_context(&stage).with_single_origin(InvocationOrigin::App {
        app_id: "app-1".into(),
        attachment_id: None,
        connection_epoch: None,
        server_generation: 3,
    });

    // 单条入口来源整批一致，不因模型 tool-call ID 而改变。
    assert_eq!(
        context.origin_for(&ToolCallId::new("call-1")),
        InvocationOrigin::App {
            app_id: "app-1".into(),
            attachment_id: None,
            connection_epoch: None,
            server_generation: 3,
        }
    );
    assert_eq!(context.session_id(), Some("session-7"));
}

/// [回归] idle 来源**没有 turn**（事实缺失，不是被清空），需要 turn 的规则返回
/// **typed** 不可用（带可诊断字段），而不是静默跳过、也不是泛化 `Other` 错误；
/// 与此同时 App / Host 来源的准入规则仍可运行（它们的输入全是 session 级事实）。
#[test]
fn idle_source_has_no_turn_and_typed_rules_fail_closed() {
    let stage = test_stage();
    stage
        .session
        .session_context
        .write()
        .insert("session_id".into(), "session-idle".into());
    let context = idle_context(&stage);

    assert!(context.turn().is_none(), "idle 入口没有 turn 事实");
    assert_eq!(
        context.attribution(),
        Some(&InteractionAttribution::app_attachment(
            "app-idle",
            "att-idle",
            peri_acp_types::interaction::ConnectionEpoch::initial()
        )),
        "idle 归属由 typed 来源单一派生"
    );
    assert!(
        !context.has_render_events(),
        "idle 执行按构造没有 render 事件端口（不冒充模型工具调用）"
    );

    // 需要 turn 的规则：显式不可用，且可诊断（入口名 + 已知会话身份）。
    let error = context
        .require_turn("before_tools_batch")
        .expect_err("无 turn 必须显式拒绝");
    let AgentError::NotAvailableWithoutTurn { entry, session_id } = error else {
        panic!("必须是 typed 不可用错误，实际: {error:?}");
    };
    assert_eq!(entry, "before_tools_batch", "错误必须点名准入入口");
    assert_eq!(
        session_id.as_deref(),
        Some("session-idle"),
        "已知的会话身份必须可诊断"
    );

    // App 来源不是模型来源：管线允许 `turn: None`，规则照常可运行。
    assert!(
        !context.requires_model_turn(),
        "App 来源的 idle 执行不要求 turn"
    );
    assert!(
        context
            .require_model_turn("tool dispatch")
            .expect("App 来源缺 turn 不是错误")
            .is_none(),
        "App 来源照常执行，只是没有 turn 绑定的事实"
    );

    // 模型来源的 idle 组合不成立：typed 不可用（逐条报告，不静默放行）。
    let model_idle = InvocationContext::from_source(idle_source(
        &stage,
        InvocationOrigin::Model {
            tool_call_id: ToolCallId::new("call-1"),
        },
    ));
    let AgentError::NotAvailableWithoutTurn { entry, .. } = model_idle
        .require_model_turn("tool dispatch")
        .expect_err("模型来源无 turn 必须显式拒绝")
    else {
        panic!("必须是 typed 不可用错误");
    };
    assert_eq!(entry, "tool dispatch");

    // 无会话身份时同样可诊断（idle 入口可能连 session 身份都没有）。
    let bare = {
        let stage = test_stage();
        InvocationContext::from_source(idle_source(
            &stage,
            InvocationOrigin::Model {
                tool_call_id: ToolCallId::new("call-1"),
            },
        ))
    };
    let AgentError::NotAvailableWithoutTurn { session_id, .. } = bare
        .require_turn("tool dispatch")
        .expect_err("无 turn 必须显式拒绝")
    else {
        panic!("必须是 typed 不可用错误");
    };
    assert_eq!(session_id, None);
}
