use super::*;

#[test]
fn sdk_execution_identity_is_immutable_and_replaces_local_ids() {
    use peri_acp_types::session_resources::{work::WorkAdmission, ControlAttempt};
    let context = TurnContext::new(Arc::from("/tmp"), Arc::new(CancellationToken::new()));
    let local = context.execution_binding();
    let admission = WorkAdmission {
        session_id: "owned-session".to_owned(),
        admission_id: "registered-admission".to_owned(),
        instance_id: "sdk-instance".to_owned(),
        generation_id: "sdk-generation".to_owned(),
        lifecycle: 2,
        control_generation: 7,
        work_id: "accepted-work".to_owned(),
        work_revision: 3,
        execution: ControlAttempt {
            turn_id: TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        },
    };
    assert!(context.bind_work_admission(admission.clone()));
    assert!(context.bind_work_admission(admission.clone()));
    assert_ne!(context.execution_binding(), local);
    assert_eq!(context.turn_id(), admission.execution.turn_id);
    assert_eq!(
        context.execution_binding().attempt_id,
        admission.execution.attempt_id
    );
    let mut replacement = admission.clone();
    replacement.execution.attempt_id = peri_acp_types::identity::AttemptId::new();
    assert!(!context.bind_work_admission(replacement));
    assert_eq!(context.work_admission(), Some(&admission));
    assert!(context.bind_control_generation(7));
    assert!(context.bind_control_generation(7));
    assert!(!context.bind_control_generation(8));
}

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
