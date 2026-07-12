use super::*;

#[test]
fn test_turn_id_unique_and_ordered() {
    let id1 = TurnId::new();
    let id2 = TurnId::new();
    assert_ne!(id1, id2, "TurnId 必须唯一");
    // UUID v7 时间有序：后创建的应大于等于先创建的
    assert!(id2.as_uuid() >= id1.as_uuid(), "TurnId 应时间有序");
}

#[test]
fn test_turn_context_step_advances() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let token = Arc::new(CancellationToken::new());
    let ctx = TurnContext::new(cwd, token);

    assert_eq!(ctx.current_step(), 0);
    assert_eq!(ctx.advance_step(), 1);
    assert_eq!(ctx.current_step(), 1);
    assert_eq!(ctx.advance_step(), 2);
    assert_eq!(ctx.current_step(), 2);
}

#[test]
fn test_turn_context_cancel_propagates() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let token = Arc::new(CancellationToken::new());
    let ctx = TurnContext::new(cwd, token.clone());

    assert!(!ctx.is_cancelled());
    token.cancel();
    assert!(ctx.is_cancelled());
}

#[test]
fn test_turn_context_child_token_cascades() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let token = Arc::new(CancellationToken::new());
    let ctx = TurnContext::new(cwd, token.clone());

    let child = ctx.child_token();
    assert!(!child.is_cancelled());
    token.cancel();
    assert!(child.is_cancelled(), "子 token 应跟随父 token 取消");
}
