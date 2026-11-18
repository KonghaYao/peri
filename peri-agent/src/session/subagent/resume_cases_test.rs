//! Subagent resume behavior cases.

use crate::agent::async_tasks::BackgroundTask;
use crate::agent::async_tasks::BackgroundTaskStatus;
use crate::agent::async_tasks::BgCancelHandle;
use crate::agent::async_tasks::BgTaskKind;
use crate::agent::async_tasks::TaskManager;
use crate::messages::BaseMessage;
use crate::messages::ToolCallRequest;
use crate::session::subagent::tests::CancelGateLLM;
use crate::session::subagent::tests::EchoLLM;
use crate::session::subagent::tests::GateLLM;
use crate::session::subagent::tests::RecordingLLM;
use crate::session::subagent::types::SubagentHost;
use crate::session::subagent::util::extract_last_ai_text;
use crate::session::subagent::SessionFactory;
use crate::session::test_resources::mock::MockSessionResources;
use crate::session::FrozenContext;
use crate::session::Session;
use crate::thread::ThreadMeta;
use peri_acp_types::thread::AgentStatus;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_resume_subagent_invalid_thread_id_rejected() {
    let store = MockSessionResources::new();
    let config = crate::session::subagent::tests::resume_config(
        Arc::clone(&store),
        "not-a-uuid".to_string(),
    );
    let err = crate::session::subagent::tests::resume_err(None, config).await;
    assert_eq!(err, "resume_subagent: invalid thread id: not-a-uuid");
}

/// resume_subagent：校验分支 1——thread 不存在 → Err
#[tokio::test]
async fn test_resume_subagent_thread_not_found() {
    let store = MockSessionResources::new();
    // 合法 UUID 但未创建（low-1 后非 UUID 会先被格式校验拦截，测不到 not found）
    let id = uuid::Uuid::now_v7().to_string();
    let config = crate::session::subagent::tests::resume_config(Arc::clone(&store), id.clone());
    let err = crate::session::subagent::tests::resume_err(None, config).await;
    assert_eq!(err, format!("resume_subagent: thread not found: {}", id));
}

/// resume_subagent：校验分支 2——agent_status 为 active（未正常收尾）→ Err；
/// update_thread_status 置 done 后 load_meta 读回新状态（R-L2），恢复可通过校验
/// 并完整执行
#[tokio::test]
async fn test_resume_subagent_active_thread_rejected() {
    let store = MockSessionResources::new();
    let id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = id.clone();
    meta.parent_thread_id = Some("parent-thread-1".to_string());
    store
        .create_resumable_thread(meta)
        .await
        .expect("the create resumable thread fixture result is required for this assertion");

    // 预置 active（ThreadMeta 默认）→ 拒绝
    let config = crate::session::subagent::tests::resume_config(Arc::clone(&store), id.clone());
    let err = crate::session::subagent::tests::resume_err(None, config).await;
    assert_eq!(
        err,
        format!(
            "resume_subagent: thread {} is still active \
            (thread 仍处于运行态: 可能仍在执行, 或上次异常退出未收尾; \
            若确认无执行中任务, 可改用 Agent(subagent_type: ...) 新建)",
            id
        )
    );

    // update_thread_status → load_meta 读回新状态（R-L2：mock 同步 agent_status）
    store
        .update_thread_status(&id, "done")
        .await
        .expect("the update thread status fixture result is required for this assertion");
    let meta = store
        .load_meta(&id)
        .await
        .expect("the load meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, AgentStatus::Done);

    // 非 active 后校验通过 → 完整执行（EchoLLM 完成 → 收尾 done）
    let config = crate::session::subagent::tests::resume_config(Arc::clone(&store), id.clone());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("非 active 后可恢复");
    assert_eq!(spawned.child_thread_id, id);
    crate::session::subagent::tests::wait_resume_settled(&store, &id).await;
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复执行完成后收尾 done"
    );
}

#[tokio::test]
async fn test_resume_subagent_legacy_history_starts_a_fresh_run_without_runtime_metadata() {
    let store = MockSessionResources::new();
    let id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = id.clone();
    meta.parent_thread_id = Some("other-parent".to_string()); // 与父 session 不一致
    store
        .create_legacy_thread(meta)
        .await
        .expect("the create legacy thread fixture result is required for this assertion");
    store
        .update_thread_status(&id, "done")
        .await
        .expect("the update thread status fixture result is required for this assertion");
    store
        .append_message(&id, BaseMessage::system("LEGACY_SYSTEM_NOT_CHILD_IDENTITY"))
        .await
        .expect("the append message fixture result is required for this assertion");

    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        Some("parent-thread-2".into()),
    );
    let llm = RecordingLLM::new();
    let calls = llm.received.clone();
    let manager = Arc::new(TaskManager::new());
    let config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(llm),
            "fixture-scripted",
        ),
        Some(manager.clone()),
        None,
    );
    let resumed = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("the resume subagent fixture result is required for this assertion");
    assert_eq!(resumed.child_thread_id, id);
    crate::session::subagent::tests::wait_background_idle(&manager).await;
    assert!(
        resumed.session.store().frozen.system_prompt.is_empty(),
        "非 hidden legacy 历史的首条 System 不得升级为子身份"
    );
    assert_eq!(calls.read().len(), 1);
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "拒绝执行后必须回滚准备阶段认领"
    );
}

/// resume_subagent：主 agent 场景——TUI 主 session 的 `store().thread_id` 恒为
/// None（parent id 仅经 `SubagentHost.parent_thread_id` 注入），resume 成功。
/// （parent 链校验已移除，本测试保留为主 agent 路径的恢复成功回归）
#[tokio::test]
async fn test_resume_subagent_main_agent_via_host_parent_id() {
    let store = MockSessionResources::new();
    let id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(
        &store,
        &id,
        Some("main-context-thread"),
    )
    .await;

    // 主 agent 样子：store().thread_id = None + host.parent_thread_id = ctx.thread_id
    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        None,
    );
    parent.set_subagent_host(SubagentHost {
        parent_thread_id: Some("main-context-thread".to_string()),
        ..Default::default()
    });

    let config = crate::session::subagent::tests::resume_config(Arc::clone(&store), id.clone());
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("主 agent 场景恢复应成功");
    assert_eq!(spawned.child_thread_id, id, "thread_id 不变");
    crate::session::subagent::tests::wait_resume_settled(&store, &id).await;
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复完成后收尾 done"
    );
}

/// resume_subagent：校验全部通过 → 重建 + 完整执行（thread_id 不变）
#[tokio::test]
async fn test_resume_subagent_validation_passes_and_runs() {
    let store = MockSessionResources::new();
    let id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = id.clone();
    meta.parent_thread_id = Some("parent-thread-3".to_string());
    store
        .create_resumable_thread(meta)
        .await
        .expect("the create resumable thread fixture result is required for this assertion");
    store
        .update_thread_status(&id, "done")
        .await
        .expect("the update thread status fixture result is required for this assertion");

    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        Some("parent-thread-3".into()),
    );
    let config = crate::session::subagent::tests::resume_config(store.clone(), id.clone());
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("校验通过后恢复执行");
    assert_eq!(spawned.child_thread_id, id, "thread_id 不变");
    crate::session::subagent::tests::wait_resume_settled(&store, &id).await;
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复完成后收尾 done"
    );
}

/// 重建正确性：transcript 完整重放（消息数/顺序）、thread_id 不变、
/// status 状态机 done → active → done、cwd 取 meta.cwd、frozen 从父 copy
#[tokio::test]
async fn test_resume_subagent_replays_transcript_and_preserves_thread_id() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    let parent_id = "parent-thread-r1";
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, Some(parent_id))
        .await;
    let original_msgs = vec![
        BaseMessage::human("task-1"),
        BaseMessage::ai("answer-1"),
        BaseMessage::human("task-2"),
    ];
    store
        .append_messages(&thread_id, &original_msgs)
        .await
        .expect("the append messages fixture result is required for this assertion");

    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder()
            .claude_md("frozen-claude")
            .skill_summary("frozen-skills")
            .date("2026-08-05")
            .build(),
        Some(parent_id.into()),
    );
    let config = crate::session::subagent::tests::resume_config(store.clone(), thread_id.clone());
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("resume ok");
    crate::session::subagent::tests::wait_resume_settled(&store, &thread_id).await;

    // transcript 完整重放（顺序断言：旧消息 → 隐式 continue prompt → AI echo）
    let tx = spawned.session.transcript();
    let guard = tx.read();
    let msgs: Vec<BaseMessage> = guard.visible_messages().into_iter().cloned().collect();
    assert_eq!(msgs.len(), 5, "3 条旧消息 + prompt + echo");
    assert_eq!(msgs[0].content(), "task-1");
    assert_eq!(msgs[1].content(), "answer-1");
    assert_eq!(msgs[2].content(), "task-2");
    assert_eq!(
        msgs[3].content(),
        "Continue your previous task where you left off.",
        "prompt 缺省注入隐式 continue 常量"
    );
    assert_eq!(
        msgs[4].content(),
        "echo: Continue your previous task where you left off.",
        "EchoLLM 消费 queue 中 prompt 后回显"
    );

    // thread_id 不变（= 恢复目标，不新建）
    assert_eq!(spawned.child_thread_id, thread_id);
    assert_eq!(
        spawned.session.store().thread_id.as_deref(),
        Some(thread_id.as_str()),
        "重建 session 的 thread_id 固定为恢复目标"
    );

    // cwd 取 meta.cwd（thread 创建时固化），frozen 从父 copy（ARC-FROZEN-001）
    assert_eq!(spawned.session.store().cwd.as_ref(), "/tmp/work");
    let child_frozen = &spawned.session.store().frozen;
    assert_eq!(child_frozen.claude_md.as_ref(), "frozen-claude");
    assert_eq!(child_frozen.skill_summary.as_ref(), "frozen-skills");
    assert_eq!(child_frozen.date.as_ref(), "2026-08-05");

    // status 状态机：预置 done → 恢复置 active → 完成收尾 done
    let statuses = store.statuses();
    let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(seq, vec!["done", "active", "done"], "status 状态机完整");
}

/// 末条截断（R2-MID-1）：末条为含未配对 tool_calls 的 AI → pop——
/// 不回放进 transcript、不发给 LLM；已配对轮次保留
#[tokio::test]
async fn test_resume_subagent_pops_unpaired_tool_call_ai() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;

    // 身份持久形态：spawn 在子会话 own history 起始写入身份 System（见 spawn
    // 6b），恢复路径读回该条、不再重注入。夹具按同一形态预置。
    let identity = BaseMessage::system("CHILD_IDENTITY_SENTINEL");

    // 完整配对轮次 + 末条未配对 AI（崩溃窗口残留形态：AI 已落盘、Tool 未落盘）
    let paired_ai = BaseMessage::ai_with_tool_calls(
        "paired-think",
        vec![ToolCallRequest::new(
            "t1",
            "read_file",
            serde_json::json!({}),
        )],
    );
    let unpaired_ai = BaseMessage::ai_with_tool_calls(
        "unpaired-think",
        vec![ToolCallRequest::new(
            "t2",
            "read_file",
            serde_json::json!({}),
        )],
    );
    let tool_result = BaseMessage::tool_result("t1", "ok");
    store
        .append_messages(
            &thread_id,
            &[
                identity.clone(),
                BaseMessage::human("task"),
                paired_ai.clone(),
                tool_result.clone(),
                unpaired_ai.clone(),
            ],
        )
        .await
        .expect("the append messages fixture result is required for this assertion");

    let llm = RecordingLLM::new();
    let manager = Arc::new(TaskManager::new());
    let config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(llm.clone()),
            "fixture-scripted",
        ),
        Some(manager.clone()),
        None,
    );
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("resume ok");
    crate::session::subagent::tests::wait_background_idle(&manager).await;

    // transcript 层面：末条未配对 AI 被 pop，已配对轮次保留
    let tx = spawned.session.transcript();
    let guard = tx.read();
    let msgs: Vec<BaseMessage> = guard.visible_messages().into_iter().cloned().collect();
    assert!(
        !msgs.iter().any(|m| m.id() == unpaired_ai.id()),
        "末条含 tool_calls 的 AI 必须被 pop"
    );
    assert!(
        msgs.iter().any(|m| m.id() == paired_ai.id()),
        "已配对轮次的 AI 保留"
    );
    assert!(
        msgs.iter().any(|m| m.id() == tool_result.id()),
        "已配对轮次的 Tool 结果保留"
    );

    // LLM 视角：同样不含被 pop 消息（且收到重放 + prompt）。
    // Model 请求面不含 transcript message id：按内容锚定同一批消息。
    let received = llm.received.read();
    assert_eq!(received.len(), 1, "单轮 LLM 调用");
    assert!(
        !received[0]
            .iter()
            .any(|m| m.content().contains("unpaired-think")),
        "被 pop 的消息不得发给 LLM"
    );
    assert!(
        received[0]
            .iter()
            .any(|m| m.content().contains("paired-think")),
        "已配对轮次发给 LLM（重放语义）"
    );
    // H1：身份自子会话持久历史读回（bridge 不重注入），故请求面为
    // system(身份) + human + paired AI + tool result + prompt。
    assert!(
        matches!(received[0].first(), Some(BaseMessage::System { .. })),
        "身份必须作为请求首条 system 出现: {:?}",
        received[0]
    );
    assert_eq!(
        received[0]
            .iter()
            .filter(|m| m.content().contains("CHILD_IDENTITY_SENTINEL"))
            .count(),
        1,
        "恢复身份恰一次（来自持久历史）: {:?}",
        received[0]
    );
    assert_eq!(
        received[0].len(),
        5,
        "system(身份) + human + paired AI + tool result + prompt: {:?}",
        received[0]
    );
}

/// 末条保留（R2-MID-1）：完整配对轮次（末条 = Tool）→ 不 pop，
/// 已完成轮次（含副作用）完整重放，避免 LLM 重复执行工具副作用
#[tokio::test]
async fn test_resume_subagent_keeps_complete_tool_round() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;

    let paired_ai = BaseMessage::ai_with_tool_calls(
        "paired-think",
        vec![ToolCallRequest::new(
            "t1",
            "read_file",
            serde_json::json!({}),
        )],
    );
    let tool_result = BaseMessage::tool_result("t1", "ok");
    store
        .append_messages(
            &thread_id,
            &[
                BaseMessage::human("task"),
                paired_ai.clone(),
                tool_result.clone(),
            ],
        )
        .await
        .expect("the append messages fixture result is required for this assertion");

    let config = crate::session::subagent::tests::resume_config(store.clone(), thread_id.clone());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("resume ok");
    crate::session::subagent::tests::wait_resume_settled(&store, &thread_id).await;

    let tx = spawned.session.transcript();
    let guard = tx.read();
    let msgs: Vec<BaseMessage> = guard.visible_messages().into_iter().cloned().collect();
    assert!(
        msgs.iter().any(|m| m.id() == paired_ai.id()),
        "末条为 Tool 时不得 pop 其前的 AI（完整配对轮次保留）"
    );
    assert!(
        msgs.iter().any(|m| m.id() == tool_result.id()),
        "末条 Tool 保留"
    );
    assert_eq!(msgs.len(), 5, "human + AI + tool + prompt + echo");
}

/// prompt 两分支（显式）：resume 带新 prompt → 原样追加为 Human 指令
/// （不套 fork directive），EchoLLM 消费并回显；不注入隐式 continue
#[tokio::test]
async fn test_resume_subagent_new_prompt_appended() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("old-task")])
        .await
        .expect("the append messages fixture result is required for this assertion");

    let mut config =
        crate::session::subagent::tests::resume_config(store.clone(), thread_id.clone());
    config.prompt = Some("do the new thing".to_string());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("resume ok");
    crate::session::subagent::tests::wait_resume_settled(&store, &thread_id).await;

    let tx = spawned.session.transcript();
    let guard = tx.read();
    let msgs: Vec<BaseMessage> = guard.visible_messages().into_iter().cloned().collect();
    assert!(
        msgs.iter().any(|m| m.content() == "do the new thing"),
        "新 prompt 原样追加进 transcript"
    );
    let last_ai = extract_last_ai_text(&spawned.session);
    assert!(
        last_ai.contains("do the new thing"),
        "追加指令被 LLM 消费，got: {}",
        last_ai
    );
    assert!(
        !last_ai.contains("Continue your previous task"),
        "显式 prompt 时不注入隐式 continue"
    );
}

#[tokio::test]
async fn test_resume_subagent_interrupted_then_manual_history_continue() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("task")])
        .await
        .expect("the append messages fixture result is required for this assertion");

    // 第一次恢复：进入 Reason 后取消，覆盖 stage-local Interrupted 规范化。
    let token = CancellationToken::new();
    let (gate, entered_rx) = CancelGateLLM::new();
    let manager = Arc::new(TaskManager::new());
    let config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(gate),
            "fixture-scripted",
        ),
        Some(manager.clone()),
        Some(token.clone()),
    );
    let spawned1 = SessionFactory::resume_subagent(None, config)
        .await
        .expect("resume 1 ok（中断不是 Err）");
    assert_eq!(spawned1.child_thread_id, thread_id);
    entered_rx
        .await
        .expect("background subagent 必须进入 Reason");
    token.cancel();
    crate::session::subagent::tests::wait_resume_settled(&store, &thread_id).await;
    crate::session::subagent::tests::wait_background_idle(&manager).await;
    {
        let statuses = store.statuses();
        assert_eq!(
            statuses.last().map(|(_, s)| s.as_str()),
            Some("cancelled"),
            "A3：真实取消的中断收尾写 cancelled"
        );
    }

    let llm = RecordingLLM::new();
    let calls = llm.received.clone();
    let manager = Arc::new(TaskManager::new());
    let config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(llm),
            "fixture-scripted",
        ),
        Some(manager.clone()),
        None,
    );
    let resumed = SessionFactory::resume_subagent(None, config)
        .await
        .expect("the resume subagent fixture result is required for this assertion");
    assert_eq!(resumed.child_thread_id, thread_id);
    crate::session::subagent::tests::wait_background_idle(&manager).await;
    assert_eq!(calls.read().len(), 1);
}

/// 并发 resume 互斥（R-M1）：两个任务同时 resume 同一 thread_id，
/// 仅一个成功进入执行（第二个在锁内看到 active 被拒）
#[tokio::test]
async fn test_resume_subagent_concurrent_resume_mutex() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;

    let (gate, release_tx) = GateLLM::new();
    let manager = Arc::new(TaskManager::new());
    let store1 = Arc::clone(&store);
    let thread_id1 = thread_id.clone();
    let gate1 = gate.clone();
    let manager1 = manager.clone();
    let t1 = tokio::spawn(async move {
        let config = crate::session::subagent::tests::resume_config_with(
            store1.clone(),
            thread_id1,
            crate::session::test_resources::mock::model::fixture_source(
                std::sync::Arc::new(gate1.clone()),
                "fixture-scripted",
            ),
            Some(manager1),
            None,
        );
        SessionFactory::resume_subagent(None, config).await
    });

    // 等待 t1 完成「校验 → 置 active」（锁内置位；随后 t1 进入执行并被 gate 挂起）
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if store.statuses().iter().any(|(_, s)| s.as_str() == "active") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("t1 应完成置 active");
    let spawned = t1.await.expect("t1 task ok").expect("t1 resume ok");

    // 第二个并发 resume：锁内看到 active → 拒绝
    let store2 = Arc::clone(&store);
    let thread_id2 = thread_id.clone();
    let t2 = tokio::spawn(async move {
        let config = crate::session::subagent::tests::resume_config(store2.clone(), thread_id2);
        SessionFactory::resume_subagent(None, config).await
    });
    let t2_res = t2.await.expect("t2 task ok");
    match t2_res {
        Err(e) => assert!(
            e.to_string().contains("still active"),
            "并发 resume 必须被 active 拒绝，got: {}",
            e
        ),
        Ok(_) => panic!("第二个并发 resume 不得进入执行（R-M1 互斥）"),
    }

    // 放行 t1 → 完成（oneshot 有缓冲，send 先于 LLM await 也不丢）
    let _ = release_tx.send(());
    crate::session::subagent::tests::wait_background_idle(&manager).await;
    assert_eq!(spawned.child_thread_id, thread_id);
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "胜出方正常收尾 done"
    );
}

/// 重建失败回滚（R-M1）：load_messages 失败 → status 回滚至原值
/// （不被 active 卡死，可再次恢复）
#[tokio::test]
async fn test_resume_subagent_rolls_back_status_on_rebuild_failure() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;

    // 注入「认领后 history 装载」失败：绑定预检只读 binding，不经过完整快照；
    // 因此第 1 次完整快照读取就是 own history 装载。
    store.fail_snapshot_load_at(1);
    let config = crate::session::subagent::tests::resume_config(store.clone(), thread_id.clone());
    let err = crate::session::subagent::tests::resume_err(None, config).await;
    assert!(
        err.contains("failed to load messages"),
        "重建失败错误必须带原因，got: {}",
        err
    );

    // status 回滚至原值（done），未被 active 卡死
    let meta = store
        .load_meta(&thread_id)
        .await
        .expect("the load meta fixture result is required for this assertion");
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "重建失败必须回滚 status 至原值"
    );
    {
        let statuses = store.statuses();
        let seq: Vec<&str> = statuses.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(seq, vec!["done", "active", "done"], "active 后回滚原值");
    }

    // 回滚后可再次恢复成功（不残留互斥态）
    let config = crate::session::subagent::tests::resume_config(store.clone(), thread_id.clone());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("回滚后可再次恢复");
    assert_eq!(spawned.child_thread_id, thread_id);
    crate::session::subagent::tests::wait_resume_settled(&store, &thread_id).await;
}

/// parent None 组合：meta.parent_thread_id = Some(x) 且调用方无 parent session
/// （/bg 命令等路径）→ 恢复成功（无 parent 链校验，仅存在性 + status 校验）
#[tokio::test]
async fn test_resume_subagent_explicit_delegation_runs_without_parent_session_handle() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    // meta 声明了父链，但调用方无 parent session（/bg 命令等路径）
    crate::session::subagent::tests::preset_resumable_thread(
        &store,
        &thread_id,
        Some("orphan-parent"),
    )
    .await;
    store
        .append_messages(&thread_id, &[BaseMessage::human("task")])
        .await
        .expect("the append messages fixture result is required for this assertion");

    let config = crate::session::subagent::tests::resume_config(store.clone(), thread_id.clone());
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("parent None 时无 parent 链校验，恢复成功");
    assert_eq!(spawned.child_thread_id, thread_id);
    crate::session::subagent::tests::wait_resume_settled(&store, &thread_id).await;
    let statuses = store.statuses();
    assert_eq!(
        statuses.last().map(|(_, s)| s.as_str()),
        Some("done"),
        "恢复完成后收尾 done"
    );
}
include!("resume_background_cases_test.rs");

/// [回归测试] 绑定不匹配必须在 binding 预检阶段拒绝：不读取任何完整快照，
/// 也不改变 child 的 agent_status。
#[tokio::test]
async fn test_resume_binding_mismatch_rejected_from_binding_only() {
    let store = MockSessionResources::new();
    let child_id = uuid::Uuid::now_v7().to_string();
    store.register_bound_session(&child_id, "/tmp/child-workspace");
    store
        .update_thread_status(&child_id, "done")
        .await
        .expect("the update thread status fixture result is required for this assertion");
    store
        .append_message(&child_id, BaseMessage::human("task"))
        .await
        .expect("the append message fixture result is required for this assertion");

    let parent_id = uuid::Uuid::now_v7().to_string();
    // 两次登记各自生成独立 binding（project/workspace 不同）→ 必然 mismatch。
    store.register_bound_session(&parent_id, "/tmp/parent-workspace");
    let parent = Session::new(
        Arc::from("/tmp/parent-workspace"),
        FrozenContext::builder().build(),
        Some(parent_id),
    );

    let config = crate::session::subagent::tests::resume_config(store.clone(), child_id.clone());
    let err = crate::session::subagent::tests::resume_err(Some(&parent), config).await;
    assert!(
        err.contains("execution binding does not match"),
        "binding mismatch 应在预检拒绝，got: {err}"
    );
    assert_eq!(
        store.snapshot_reads(),
        0,
        "binding 预检不得为校验物化 child/parent 有效正文"
    );
    assert_eq!(
        store
            .load_meta(&child_id)
            .await
            .expect("the load meta fixture result is required for this assertion")
            .agent_status,
        AgentStatus::Done,
        "预检拒绝不得认领或改写 child 状态"
    );
}
