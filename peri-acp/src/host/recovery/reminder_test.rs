//! 恢复提醒中的外围任务事实：`Known` 列数量与状态，`Unknown` 如实说明无法确认。

use super::*;
use peri_acp_types::ports::{LiveTask, PeripheralTasks};

fn body_for(peripheral_tasks: &PeripheralTasks) -> String {
    let root = "root-thread".to_string();
    let thread = root.clone();
    recovery_reminder(&root, &thread, &[], peripheral_tasks)
        .expect("恢复提醒必须可构造")
        .as_reminder()
        .body
        .clone()
}

#[test]
fn known_peripheral_tasks_list_count_and_states() {
    let body = body_for(&PeripheralTasks::Known(vec![
        LiveTask {
            task_id: "shell-1".into(),
            owner: Some("thread-a".into()),
            state: "working".into(),
        },
        LiveTask {
            task_id: "shell-2".into(),
            owner: Some("thread-a".into()),
            state: "input_required".into(),
        },
    ]));
    assert!(body.contains("外围任务：2 个仍存活"), "正文: {body}");
    assert!(body.contains("shell-1：working"), "正文: {body}");
    assert!(body.contains("shell-2：input_required"), "正文: {body}");
    assert!(!body.contains("无法确认"), "已知时不得写成未知: {body}");
}

#[test]
fn known_empty_peripheral_tasks_says_none_alive() {
    let body = body_for(&PeripheralTasks::Known(Vec::new()));
    assert!(
        body.contains("已确认当前没有仍存活的 Shell 任务"),
        "正文: {body}"
    );
}

#[test]
fn unknown_peripheral_tasks_say_they_cannot_be_confirmed() {
    let body = body_for(&PeripheralTasks::unknown("workspace not connected"));
    assert!(
        body.contains("外围任务归属与存活状态：未知"),
        "正文: {body}"
    );
    assert!(
        body.contains("无法确认外围任务是否仍在运行"),
        "未知必须说明无法确认: {body}"
    );
    assert!(
        !body.contains("没有仍存活的 Shell 任务"),
        "未知不得写成没有任务: {body}"
    );
}
