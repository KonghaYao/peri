use chrono::Timelike;
use tokio::sync::mpsc;

use super::*;

fn new_scheduler() -> (CronScheduler, mpsc::UnboundedReceiver<CronTrigger>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (CronScheduler::new(tx), rx)
}

#[test]
fn test_register_valid() {
    let (mut sched, _rx) = new_scheduler();
    let id = sched.register("* * * * *", "test prompt").unwrap();
    assert!(!id.is_empty());
    let task = sched.get_task(&id).unwrap();
    assert_eq!(task.expression, "* * * * *");
    assert_eq!(task.prompt, "test prompt");
    assert!(task.enabled);
    assert!(task.next_fire.is_some());
}

#[test]
fn test_register_invalid_expression() {
    let (mut sched, _rx) = new_scheduler();
    let result = sched.register("invalid", "test");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("cron 表达式无效"));
}

#[test]
fn test_remove() {
    let (mut sched, _rx) = new_scheduler();
    let id = sched.register("* * * * *", "test").unwrap();
    assert!(sched.remove(&id));
    assert!(!sched.remove(&id));
    assert!(sched.get_task(&id).is_none());
}

#[test]
fn test_toggle() {
    let (mut sched, _rx) = new_scheduler();
    let id = sched.register("* * * * *", "test").unwrap();
    assert!(sched.toggle(&id));
    let task = sched.get_task(&id).unwrap();
    assert!(!task.enabled);
    assert!(sched.toggle(&id));
    let task = sched.get_task(&id).unwrap();
    assert!(task.enabled);
    assert!(task.next_fire.is_some());
}

#[test]
fn test_toggle_nonexistent() {
    let (mut sched, _rx) = new_scheduler();
    assert!(!sched.toggle("nonexistent"));
}

#[test]
fn test_max_tasks() {
    let (mut sched, _rx) = new_scheduler();
    // croner 6-field format: use 5-field standard cron
    for i in 0..20 {
        let expr = "* * * * *".to_string();
        sched.register(&expr, &format!("task {}", i)).unwrap();
    }
    let result = sched.register("* * * * *", "overflow");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("上限"));
}

#[test]
fn test_tick_fires_trigger() {
    let (mut sched, mut rx) = new_scheduler();
    // Register with a cron that already passed - we manually set next_fire to past
    let id = sched.register("* * * * *", "tick test").unwrap();
    // Force next_fire to the past
    let task = sched.tasks.get_mut(&id).unwrap();
    task.next_fire = Some(Utc::now() - chrono::Duration::seconds(10));

    sched.tick();

    let trigger = rx.try_recv().unwrap();
    assert_eq!(trigger.task_id, id);
    assert_eq!(trigger.prompt, "tick test");

    // next_fire should be updated to future
    let task = sched.get_task(&id).unwrap();
    assert!(task.next_fire.unwrap() > Utc::now() - chrono::Duration::seconds(5));
}

#[test]
fn test_tick_skips_disabled() {
    let (mut sched, mut rx) = new_scheduler();
    let id = sched.register("* * * * *", "skip test").unwrap();
    sched.toggle(&id); // disable
    sched.tick();
    assert!(rx.try_recv().is_err());
}

#[test]
fn test_list_tasks() {
    let (mut sched, _rx) = new_scheduler();
    assert!(sched.list_tasks().is_empty());
    sched.register("* * * * *", "a").unwrap();
    sched.register("0 * * * *", "b").unwrap();
    assert_eq!(sched.list_tasks().len(), 2);
}

#[test]
fn test_list_tasks_sorted_by_next_fire() {
    let (mut sched, _rx) = new_scheduler();
    let id1 = sched.register("0 0 1 1 *", "yearly").unwrap();
    let id2 = sched.register("* * * * *", "minutely").unwrap();
    let tasks = sched.list_tasks();
    // minutely 应排在 yearly 前面（next_fire 更早）
    assert_eq!(tasks[0].id, id2);
    assert_eq!(tasks[1].id, id1);
}

#[test]
fn test_register_rejects_empty_prompt() {
    // 校验在 CronRegisterTool::invoke 层，scheduler.register 本身接受空 prompt
    // 此测试验证 scheduler 层不拒绝空 prompt（tools 层拒绝）
    let (mut sched, _rx) = new_scheduler();
    // scheduler.register 接受空字符串（tools 层校验 prompt 非空）
    let result = sched.register("* * * * *", "");
    assert!(result.is_ok(), "scheduler 层不应拒绝空 prompt");
}

#[test]
fn test_tick_removes_dead_extra_sender() {
    let (mut sched, _rx) = new_scheduler();
    let rx = sched.subscribe();
    drop(rx); // bridge 已死（turn 结束后 sender 失效的旧行为）
    let id = sched.register("* * * * *", "retain test").unwrap();
    sched.tasks.get_mut(&id).unwrap().next_fire = Some(Utc::now() - chrono::Duration::seconds(10));
    sched.tick();
    assert!(
        sched.extra_trigger_txs.is_empty(),
        "死 sender 应在 tick 时被 retain 清理"
    );
}

/// [回归测试] CronSchedulerPort::downcast_arc 必须还原具体实例
/// （issue 2026-08-07-cron-tool-task-never-triggers）。
///
/// 历史 bug：downcast_arc 直接对 trait object 调 `type_id()`——trait 不
/// 继承 `Any`，方法经 `Any` blanket impl 解析，返回
/// `TypeId::of::<dyn CronSchedulerPort>()`（trait object 自身），恒不等于
/// `TypeId::of::<CronSchedulerPortHandle>()` → downcast 恒失败 → 装配面
/// 回退临时 CronScheduler → cron 工具注册的 scheduler 与 host tick /
/// SessionManager bridge 订阅的 scheduler 分离，触发完全静默
/// （同构 2026-08-06-e2e-workflow-not-completing）。
#[test]
fn test_cron_scheduler_port_downcast_restores_concrete() {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use peri_acp_types::cron::CronSchedulerPort;

    let (tx, _rx) = mpsc::unbounded_channel();
    let concrete = Arc::new(Mutex::new(CronScheduler::new(tx)));
    let handle = Arc::new(CronSchedulerPortHandle(concrete.clone()));
    let port: Arc<dyn CronSchedulerPort> = handle.clone() as Arc<dyn CronSchedulerPort>;

    let restored = match Arc::clone(&port).downcast_arc::<CronSchedulerPortHandle>() {
        Ok(h) => h,
        Err(_) => panic!("downcast 必须还原具体类型 CronSchedulerPortHandle"),
    };
    assert!(
        Arc::ptr_eq(&handle, &restored),
        "还原实例必须是原 Arc（工具/订阅/tick 共享同一 scheduler）"
    );
}

fn fixed_after() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

/// [回归测试] croner 4.x 默认拒绝 3.x 接受的简写步进 `5/5`，
/// 共享 helper 必须开启 `sloppy_ranges` 恢复兼容。
#[test]
fn test_parse_cron_accepts_sloppy_step_range() {
    let cron = parse_cron("5/5 * * * *").expect("5/5 简写步进应可解析");
    let next = cron
        .iter_after(fixed_after())
        .next()
        .expect("5/5 应能计算下次触发");
    assert_eq!(
        next,
        DateTime::parse_from_rfc3339("2026-01-01T00:05:00Z")
            .unwrap()
            .with_timezone(&Utc)
    );
}

/// 注册路径（`register` 的校验解析）必须接受简写步进。
#[test]
fn test_register_accepts_sloppy_step_range() {
    let (mut sched, _rx) = new_scheduler();
    let id = sched
        .register("5/5 * * * *", "sloppy")
        .expect("注册路径应接受 5/5 简写步进");
    let task = sched.get_task(&id).unwrap();
    let next = task.next_fire.expect("注册时应计算下次触发");
    assert!(next > Utc::now());
    assert_eq!(next.minute() % 5, 0);
    assert_ne!(next.minute(), 0, "5/5 的分钟取值从 5 开始");
}

/// 下次触发计算路径（`calculate_next_fire`）必须接受简写步进。
#[test]
fn test_calculate_next_fire_accepts_sloppy_step_range() {
    let next = CronScheduler::calculate_next_fire("5/5 * * * *", fixed_after())
        .expect("下次触发计算路径应接受 5/5 简写步进");
    assert_eq!(
        next,
        DateTime::parse_from_rfc3339("2026-01-01T00:05:00Z")
            .unwrap()
            .with_timezone(&Utc)
    );
}

/// 标准 5/6 段表达式的解析与 next-trigger 行为保持不变。
#[test]
fn test_parse_cron_standard_expressions_unchanged() {
    let cases = [
        ("*/5 * * * *", "2026-01-01T00:05:00Z"),
        ("0 0 1 1 *", "2027-01-01T00:00:00Z"),
        ("0 */5 * * * *", "2026-01-01T00:05:00Z"),
    ];
    for (expression, expected) in cases {
        let cron = parse_cron(expression).unwrap_or_else(|e| panic!("{expression} 应可解析: {e}"));
        let next = cron
            .iter_after(fixed_after())
            .next()
            .unwrap_or_else(|| panic!("{expression} 应能计算下次触发"));
        assert_eq!(
            next,
            DateTime::parse_from_rfc3339(expected)
                .unwrap()
                .with_timezone(&Utc),
            "{expression}"
        );
    }
}

/// 放宽简写步进不应放行非法表达式。
#[test]
fn test_parse_cron_rejects_invalid_expression() {
    let err = parse_cron("invalid").unwrap_err();
    assert!(matches!(err, CronError::InvalidExpression(_)));
    assert!(err.to_string().contains("cron 表达式无效"));

    assert!(parse_cron("61 * * * *").is_err(), "越界分钟必须被拒绝");
}
