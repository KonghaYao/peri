use super::*;
use crate::agent::state::AgentState;
use crate::middleware::r#trait::{Middleware, NoopMiddleware};
use async_trait::async_trait;
/// 提供 first_turn_reminder 贡献的中间件（Some/None 均可配置）
struct ReminderMw {
    name: String,
    text: Option<String>,
}

impl ReminderMw {
    fn new(name: &str, text: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            text: text.map(|s| s.to_string()),
        }
    }
}

#[async_trait]
impl Middleware for ReminderMw {
    fn name(&self) -> &str {
        &self.name
    }

    async fn first_turn_reminder(
        &self,
        _state: &mut dyn hook_state::QueueState,
    ) -> AgentResult<Option<String>> {
        Ok(self.text.clone())
    }
}

/// 失败中间件：first_turn_reminder 返回错误（验证短路）
struct ReminderFailMw;

#[async_trait]
impl Middleware for ReminderFailMw {
    fn name(&self) -> &str {
        "ReminderFailMw"
    }

    async fn first_turn_reminder(
        &self,
        _state: &mut dyn hook_state::QueueState,
    ) -> AgentResult<Option<String>> {
        Err(AgentError::MiddlewareError {
            middleware: "ReminderFailMw".to_string(),
            reason: "intentional failure".to_string(),
        })
    }
}

/// 按链序收集非空贡献；None 与空白串跳过
#[tokio::test]
async fn test_first_turn_reminders_collects_in_order() {
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ReminderMw::new("A", Some("mcp overview"))));
    chain.add(Box::new(ReminderMw::new("B", None)));
    chain.add(Box::new(ReminderMw::new("C", Some("   "))));
    chain.add(Box::new(ReminderMw::new("D", Some("second notice"))));

    let mut state = AgentState::new("/tmp");
    let reminders = chain.run_first_turn_reminders(&mut state).await.unwrap();
    assert_eq!(
        reminders,
        vec!["mcp overview".to_string(), "second notice".to_string()]
    );
}

/// 空链与全部 None：返回空 vec（零噪音）
#[tokio::test]
async fn test_first_turn_reminders_none_skipped() {
    let chain = MiddlewareChain::new();
    let mut state = AgentState::new("/tmp");
    let reminders = chain.run_first_turn_reminders(&mut state).await.unwrap();
    assert!(reminders.is_empty());

    let mut chain2 = MiddlewareChain::new();
    chain2.add(Box::new(ReminderMw::new("A", None)));
    chain2.add(Box::new(ReminderMw::new("B", None)));
    let reminders = chain2.run_first_turn_reminders(&mut state).await.unwrap();
    assert!(reminders.is_empty());
}

/// 未覆盖钩子的中间件：默认实现返回 None（不报错）
#[tokio::test]
async fn test_first_turn_reminder_default_none() {
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(NoopMiddleware::new("A")));
    let mut state = AgentState::new("/tmp");
    let reminders = chain.run_first_turn_reminders(&mut state).await.unwrap();
    assert!(reminders.is_empty());
}

/// Err 短路：后续中间件不再执行
#[tokio::test]
async fn test_first_turn_reminder_error_short_circuits() {
    struct UnexpectedReminder;

    #[async_trait]
    impl Middleware for UnexpectedReminder {
        fn name(&self) -> &str {
            "UnexpectedReminder"
        }

        async fn first_turn_reminder(
            &self,
            _state: &mut dyn hook_state::QueueState,
        ) -> AgentResult<Option<String>> {
            panic!("middleware after the failure must not run")
        }
    }

    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ReminderMw::new("A", Some("ok"))));
    chain.add(Box::new(ReminderFailMw));
    chain.add(Box::new(UnexpectedReminder));
    let mut state = AgentState::new("/tmp");
    let result = chain.run_first_turn_reminders(&mut state).await;
    assert!(matches!(
        result,
        Err(AgentError::MiddlewareError { middleware, reason })
            if middleware == "ReminderFailMw" && reason == "intentional failure"
    ));
}
