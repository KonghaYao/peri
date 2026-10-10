//! [回归测试] 有界等待必须在没有任何外部唤醒时到点退出。
//!
//! 历史故障：`BoundedWait` 只有状态、没有定时器，界只在「恰好发生唤醒」
//! （任务注册/终态/取消、用户输入）时才被重新求值——parked idle 下等待
//! 仍然无界（print `sleep 100000` 200s 被 SIGKILL；`sleep 300` 在 301s
//! 任务完成时才退出）。本测试用 tokio 时间控制证明：界本身会唤醒 loop，
//! 到点即写交接并结束 turn，不依赖任何外部事件。

use super::*;
use crate::agent::async_tasks::handoff::BoundedWait;
use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as TaskManagerPort};
use std::time::Duration;

/// 构造一个「parked idle 且有一个永不结算任务」的 loop。
fn parked_context(
    bounded: std::sync::Arc<BoundedWait>,
    manager: std::sync::Arc<crate::agent::async_tasks::TaskManager>,
) -> StageContext {
    let session = crate::session::Session::new(
        Arc::from("/tmp/bounded-wait-exit"),
        crate::session::FrozenContext::builder().build(),
        None,
    );
    let wait = std::sync::Arc::clone(&bounded);
    let handoff = std::sync::Arc::clone(&bounded);
    let deadline = std::sync::Arc::clone(&bounded);
    let busy = std::sync::Arc::clone(&manager);
    let handoff_manager = std::sync::Arc::clone(&manager);
    StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_idle_waiting()
    .with_idle_registry(manager.registry().subscribe_activity())
    .with_idle_should_wait(std::sync::Arc::new(move || {
        wait.should_wait(busy.active_count() > 0)
    }))
    .with_handoff_deadline(std::sync::Arc::new(move || deadline.deadline()))
    .with_pending_handoff(std::sync::Arc::new(move || {
        if !handoff.take_due() {
            return None;
        }
        let tasks = handoff_manager.pending_handoff_tasks();
        (!tasks.is_empty()).then(|| crate::agent::async_tasks::handoff::PendingHandoff {
            tasks,
            waited: handoff.waited(),
        })
    }))
    .build()
}

fn register_never_settling_task(manager: &crate::agent::async_tasks::TaskManager) {
    TaskManagerPort::register(
        manager,
        BgTaskRegistration {
            task_id: "bg-parked".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 100000".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn test_parked_idle_exits_at_the_bound_without_any_wakeup() {
    let manager = std::sync::Arc::new(crate::agent::async_tasks::TaskManager::new());
    register_never_settling_task(&manager);
    assert!(TaskManagerPort::active_count(manager.as_ref()) > 0);
    let bounded = BoundedWait::new(Duration::from_secs(120));
    let context = parked_context(std::sync::Arc::clone(&bounded), manager);
    let flag = context.async_ctx.idle_suspended_flag.clone();
    assert!(flag.is_none(), "测试不注入 suspended 标志，保持最小接线");

    let mut handle = tokio::spawn(run_react_loop(context, 4));
    // 界内：没有任何外部唤醒时不得退出（parked）。
    assert!(
        tokio::time::timeout(Duration::from_secs(60), &mut handle)
            .await
            .is_err(),
        "界内不得退出：等待必须有界但不必提前"
    );
    // 越过 120s 界：定时器分支必须自己唤醒 loop → 写交接 → 退出。
    tokio::time::advance(Duration::from_secs(121)).await;
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("到点必须自行退出（无外部唤醒）")
        .expect("loop task 不应 panic");
    assert!(
        matches!(result, LoopResult::Completed),
        "到点退出应为正常完成，实际 {result:?}"
    );
}

// 无未结算任务时不得进入挂起：loop 立即退出，不等待 120s 界。
#[tokio::test(start_paused = true)]
async fn test_idle_without_pending_work_does_not_park() {
    let bounded = BoundedWait::new(Duration::from_secs(120));
    let session = crate::session::Session::new(
        Arc::from("/tmp/bounded-wait-no-work"),
        crate::session::FrozenContext::builder().build(),
        None,
    );
    let wait = std::sync::Arc::clone(&bounded);
    let deadline = std::sync::Arc::clone(&bounded);
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_idle_should_wait(std::sync::Arc::new(move || wait.should_wait(false)))
    .with_handoff_deadline(std::sync::Arc::new(move || deadline.deadline()))
    .build();
    let started = tokio::time::Instant::now();
    let result = run_react_loop(context, 4).await;
    assert!(matches!(result, LoopResult::Completed));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "无未结算任务时不得进入有界等待"
    );
    assert!(bounded.deadline().is_none(), "未进入等待时不存在截止窗口");
}
