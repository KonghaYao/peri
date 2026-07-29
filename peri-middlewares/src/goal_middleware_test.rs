use super::*;
use peri_agent::agent::react::AgentOutput;
use peri_agent::agent::state::AgentState;
use peri_agent::goal::{GoalController, GoalStatus, GoalViewSnapshot};
use peri_agent::middleware::r#trait::Middleware;

struct MockController {
    snapshot: parking_lot::Mutex<GoalViewSnapshot>,
}

#[async_trait]
impl GoalController for MockController {
    async fn create_goal(&self, _objective: String) -> Result<(), String> {
        Ok(())
    }
    async fn complete_goal(&self) -> Result<(), String> {
        Ok(())
    }
    async fn block_goal(&self, _reason: String) -> Result<(), String> {
        Ok(())
    }
    async fn clear_goal(&self) -> Result<(), String> {
        Ok(())
    }
    fn snapshot(&self) -> GoalViewSnapshot {
        self.snapshot.lock().clone()
    }
}

fn make_active_snapshot() -> GoalViewSnapshot {
    GoalViewSnapshot {
        objective: Some("测试目标".to_string()),
        status: Some(GoalStatus::Active),
        token_budget: None,
        tokens_used: 0,
        objective_just_updated: false,
    }
}

fn make_complete_snapshot() -> GoalViewSnapshot {
    GoalViewSnapshot {
        status: Some(GoalStatus::Complete),
        ..make_active_snapshot()
    }
}

#[test]
fn test_render_steering_escalates() {
    let r1 = GoalMiddleware::render_steering("目标", 1);
    assert!(r1.contains("Decide"));

    let r2 = GoalMiddleware::render_steering("目标", 2);
    assert!(r2.contains("must call"));

    let r3 = GoalMiddleware::render_steering("目标", 3);
    assert!(r3.contains("Attention"));
}

#[test]
fn test_render_steering_contains_objective() {
    let r = GoalMiddleware::render_steering("完成重构", 1);
    assert!(r.contains("完成重构"));
}

#[test]
fn test_collect_tools_returns_goal_tool() {
    let controller = Arc::new(MockController {
        snapshot: parking_lot::Mutex::new(make_active_snapshot()),
    }) as Arc<dyn GoalController>;
    let mw = GoalMiddleware::new(controller, None);

    let tools = Middleware::collect_tools(&mw, "/tmp");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "goal");
}

#[tokio::test]
async fn test_after_agent_goal_active_注入_steering_并设_block_continue() {
    let controller = Arc::new(MockController {
        snapshot: parking_lot::Mutex::new(make_active_snapshot()),
    }) as Arc<dyn GoalController>;
    let mw = GoalMiddleware::new(controller, None);
    let mut state = AgentState::new("/tmp");
    let output = AgentOutput::new("我完成了", 1);

    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();

    // 设 block_continue 触发 executor 续跑
    assert_eq!(result.block_continue.as_deref(), Some("goal_active"));

    // 注入路径：v2 MessageQueue 应收到 1 条 Defer（GoalSteering）
    let drained = state.v2_queue().drain_all();
    assert_eq!(drained.len(), 1, "应 push 1 条 goal steering Defer 消息");
}

#[tokio::test]
async fn test_after_agent_no_goal_放行_不注入() {
    let controller = Arc::new(MockController {
        snapshot: parking_lot::Mutex::new(GoalViewSnapshot::default()),
    }) as Arc<dyn GoalController>;
    let mw = GoalMiddleware::new(controller, None);
    let mut state = AgentState::new("/tmp");
    let output = AgentOutput::new("普通回答", 1);

    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();

    assert!(
        result.block_continue.is_none(),
        "无 goal 时不应设 block_continue"
    );
    // 无 goal 时 queue 应为空
    let drained = state.v2_queue().drain_all();
    assert!(drained.is_empty(), "无 goal 时不应 push 任何消息");
}

#[tokio::test]
async fn test_after_agent_existing_block_continue_不干预() {
    // HookMiddleware 等前置中间件已设 block_continue 时，
    // GoalMiddleware 必须尊重优先级，不覆盖也不重复注入 steering
    let controller = Arc::new(MockController {
        snapshot: parking_lot::Mutex::new(make_active_snapshot()),
    }) as Arc<dyn GoalController>;
    let mw = GoalMiddleware::new(controller, None);
    let mut state = AgentState::new("/tmp");
    let mut output = AgentOutput::new("hook 拦截", 1);
    output.block_continue = Some("hook_stop".to_string());

    let result = Middleware::after_agent(&mw, &mut state, &output)
        .await
        .unwrap();

    assert_eq!(
        result.block_continue.as_deref(),
        Some("hook_stop"),
        "已有 block_continue 应保留原值，不被 goal_active 覆盖"
    );
    // 不干预时 queue 应为空
    let drained = state.v2_queue().drain_all();
    assert!(
        drained.is_empty(),
        "已有 block_continue 时不应 push steering"
    );
}

#[tokio::test]
async fn test_after_agent_terminal_重置_pending_rounds() {
    let mock = Arc::new(MockController {
        snapshot: parking_lot::Mutex::new(make_active_snapshot()),
    });
    let controller: Arc<dyn GoalController> = mock.clone();
    let mw = GoalMiddleware::new(controller, None);
    let mut state = AgentState::new("/tmp");

    // 第一次：goal active，递增到 round 1
    let r1 = Middleware::after_agent(&mw, &mut state, &AgentOutput::new("回答1", 1))
        .await
        .unwrap();
    assert_eq!(r1.block_continue.as_deref(), Some("goal_active"));
    // v1 MessageQueue 已删除；round1 紧迫感直接由 render_steering(1) 校验
    let round1_urgency: String = GoalMiddleware::render_steering("测试目标", 1)
        .lines()
        .filter(|l| l.contains("Decide") || l.contains("must call") || l.contains("Attention"))
        .collect();

    // 转入终态（Complete）
    {
        *mock.snapshot.lock() = make_complete_snapshot();
    }
    let r2 = Middleware::after_agent(&mw, &mut state, &AgentOutput::new("完成", 2))
        .await
        .unwrap();
    assert!(r2.block_continue.is_none(), "终态时不应 block_continue");

    // 重新 active：pending_rounds 应已重置，从 round 1 重新开始
    {
        *mock.snapshot.lock() = make_active_snapshot();
    }
    let r3 = Middleware::after_agent(&mw, &mut state, &AgentOutput::new("新目标", 3))
        .await
        .unwrap();
    assert_eq!(r3.block_continue.as_deref(), Some("goal_active"));
    // v1 MessageQueue 已删除；round3 紧迫感直接由 render_steering(1) 校验
    let round3_urgency: String = GoalMiddleware::render_steering("测试目标", 1)
        .lines()
        .filter(|l| l.contains("Decide") || l.contains("must call") || l.contains("Attention"))
        .collect();

    assert_eq!(
        round1_urgency, round3_urgency,
        "终态后重新 active 应从 round 1 开始（递增紧迫感计数器已重置）"
    );

    // 验证 queue 累积：两次 active（r1 / r3）各 push 1 条 Defer，r2 终态不 push
    let drained_msg = state.v2_queue().drain_all();
    assert_eq!(
        drained_msg.len(),
        2,
        "应累积 2 条 goal steering Defer（两次 active），终态 r2 不 push"
    );
}
