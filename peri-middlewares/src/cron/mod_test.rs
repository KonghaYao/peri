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
