use peri_agent::agent::react::AgentOutput;
use peri_agent::agent::state::AgentState;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::session::MessageKind;
use tokio::sync::mpsc;

use super::*;

async fn make_mw_with_items(
    items: Vec<TodoItem>,
    require_completion: bool,
) -> (TodoMiddleware, Arc<Mutex<TodoState>>) {
    let (tx, _rx) = mpsc::channel(8);
    let mw = TodoMiddleware::new(tx);
    let state = Arc::clone(&mw.state);
    {
        let mut guard = state.lock().await;
        guard.items = items;
        guard.require_completion = require_completion;
    }
    (mw, state)
}

#[test]
fn test_render_steering_包含当前todo状态() {
    let items = vec![
        TodoItem {
            content: "重构模块".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
        TodoItem {
            content: "跑测试".into(),
            active_form: None,
            status: TodoStatus::InProgress,
        },
    ];
    let text = TodoMiddleware::render_steering(&items);
    assert!(text.contains("[0] [pending] 重构模块"));
    assert!(text.contains("[1] [in_progress] 跑测试"));
    assert!(text.contains("completed"), "应提醒标记为 completed");
    assert!(
        text.contains("requireCompletion=false"),
        "应提供显式解除的出口"
    );
}

#[tokio::test]
async fn test_after_agent_未完成_注入_steering_并设_block_continue() {
    let (mw, _state) = make_mw_with_items(
        vec![TodoItem {
            content: "A".into(),
            active_form: None,
            status: TodoStatus::Pending,
        }],
        true,
    )
    .await;
    let mut state = AgentState::new("/tmp");
    let output = AgentOutput::new("我完成了", 1);

    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();

    // 设 block_continue 触发 executor 续跑
    assert_eq!(
        result.block_continue.as_deref(),
        Some("todo_require_completion")
    );

    // 注入路径：v2 MessageQueue 应收到 1 条 Defer（TodoSteering），内容含 todo 状态
    let drained = state.v2_queue().drain_all();
    assert_eq!(drained.len(), 1, "应 push 1 条 todo steering Defer 消息");
    assert_eq!(drained[0].kind, MessageKind::Defer);
    assert_eq!(
        drained[0].source,
        MessageSource::TodoSteering,
        "来源应为 TodoSteering"
    );
    let text = drained[0].message.content().to_string();
    assert!(
        text.contains("[0] [pending] A"),
        "注入内容应含当前 todo 状态: {text}"
    );
    assert!(
        text.contains("<system-reminder>"),
        "注入必须使用 system-reminder 包裹"
    );
}

#[tokio::test]
async fn test_after_agent_全部completed_放行() {
    let (mw, _state) = make_mw_with_items(
        vec![TodoItem {
            content: "A".into(),
            active_form: None,
            status: TodoStatus::Completed,
        }],
        true,
    )
    .await;
    let mut state = AgentState::new("/tmp");
    let output = AgentOutput::new("完成", 1);

    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();

    assert!(result.block_continue.is_none(), "全部 completed 不应拦截");
    let drained = state.v2_queue().drain_all();
    assert!(drained.is_empty(), "不应注入任何消息");
}

#[tokio::test]
async fn test_after_agent_未开启标记_放行() {
    let (mw, _state) = make_mw_with_items(
        vec![TodoItem {
            content: "A".into(),
            active_form: None,
            status: TodoStatus::Pending,
        }],
        false,
    )
    .await;
    let mut state = AgentState::new("/tmp");
    let output = AgentOutput::new("普通回答", 1);

    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();

    assert!(
        result.block_continue.is_none(),
        "未开启 requireCompletion 不应拦截"
    );
    let drained = state.v2_queue().drain_all();
    assert!(drained.is_empty(), "不应注入任何消息");
}

#[tokio::test]
async fn test_after_agent_空列表_放行() {
    let (mw, _state) = make_mw_with_items(vec![], true).await;
    let mut state = AgentState::new("/tmp");
    let output = AgentOutput::new("回答", 1);

    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();

    assert!(result.block_continue.is_none(), "空列表不应拦截");
    let drained = state.v2_queue().drain_all();
    assert!(drained.is_empty(), "不应注入任何消息");
}

#[tokio::test]
async fn test_after_agent_已有block_continue_不干预() {
    let (mw, _state) = make_mw_with_items(
        vec![TodoItem {
            content: "A".into(),
            active_form: None,
            status: TodoStatus::Pending,
        }],
        true,
    )
    .await;
    let mut state = AgentState::new("/tmp");
    let mut output = AgentOutput::new("回答", 1);
    output.block_continue = Some("stop_hook_block".to_string());

    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();

    // 尊重前置中间件的优先级：保留原 block_continue，不覆盖也不重复注入
    assert_eq!(result.block_continue.as_deref(), Some("stop_hook_block"));
    let drained = state.v2_queue().drain_all();
    assert!(drained.is_empty(), "已有 block_continue 时不应注入");
}

#[tokio::test]
async fn test_after_agent_steering_解除后_不再拦截() {
    // 模拟完整生命周期：创建（标记开启）→ 全部标记完成 → 停止轮放行
    let (tx, _rx) = mpsc::channel(8);
    let mw = TodoMiddleware::new(tx);
    let mut state = AgentState::new("/tmp");

    // 经 TodoWrite 创建并开启标记
    let tool = mw.collect_tools("/tmp").remove(0);
    tool.invoke(
        serde_json::json!({
            "requireCompletion": true,
            "todos": [
                { "content": "A", "status": "in_progress" },
                { "content": "B", "status": "pending" }
            ]
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    // 停止轮 → 拦截
    let output = AgentOutput::new("先停一下", 1);
    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();
    assert_eq!(
        result.block_continue.as_deref(),
        Some("todo_require_completion"),
        "未完成时应拦截续跑"
    );
    state.v2_queue().drain_all();

    // 全部标记 completed → 停止轮放行
    tool.invoke(
        serde_json::json!({
            "todos": [
                { "content": "A", "status": "completed" },
                { "content": "B", "status": "completed" }
            ]
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let output = AgentOutput::new("全部完成", 1);
    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();
    assert!(result.block_continue.is_none(), "全部完成应放行");
    let drained = state.v2_queue().drain_all();
    assert!(drained.is_empty(), "不应再注入");
}
