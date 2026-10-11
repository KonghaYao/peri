use super::*;

fn multi_byte_prompt(bytes: usize) -> String {
    // 每个字符 3 字节，确保边界落在多字节序列中间时也不会 panic。
    "汉".repeat(bytes / 3)
}

#[test]
fn prompt_budget_is_enforced_on_both_ends_without_panicking_at_multi_byte_edges() {
    assert!(validate_cron_prompt("short task").is_ok());
    let exact = multi_byte_prompt(MAX_CRON_PROMPT_BYTES);
    assert!(exact.len() <= MAX_CRON_PROMPT_BYTES);
    assert!(validate_cron_prompt(&exact).is_ok());

    let over = multi_byte_prompt(MAX_CRON_PROMPT_BYTES + 3);
    let rejection = validate_cron_prompt(&over).expect_err("超预算必须显式拒绝");
    assert_eq!(
        rejection,
        CronPromptRejection::PromptTooLong { actual: over.len() }
    );
    assert!(
        rejection
            .to_string()
            .contains(&MAX_CRON_PROMPT_BYTES.to_string()),
        "拒绝必须给出可承载限制"
    );
    let error = cron_trigger_reminder("task-1", "firing-1", &over).expect_err("构建同样拒绝");
    assert!(matches!(error, CronTriggerReminderError::Prompt(_)));
}

#[test]
fn trigger_reminder_keeps_prompt_in_body_and_metadata_is_identity_only() {
    let prompt = multi_byte_prompt(300);
    let reminder = cron_trigger_reminder("task-7", "firing-9", &prompt).expect("可承载预算内");
    let dto = reminder.as_reminder();
    assert_eq!(
        cron_trigger_prompt(dto).as_deref(),
        Some(prompt.as_str()),
        "正文是原始指令的唯一权威编码"
    );
    assert_eq!(dto.metadata["task_id"], "task-7");
    assert_eq!(dto.metadata["firing_id"], "firing-9");
    assert_eq!(dto.metadata["prompt_bytes"], prompt.len());
    assert!(
        dto.metadata.get("prompt").is_none(),
        "metadata 不重复保存完整 prompt"
    );
    assert_eq!(
        dto.body.len(),
        prompt.len() + cron_trigger_body("task-7", "").len()
    );
    assert!(
        !dto.audiences
            .contains(crate::system_reminder::ReminderAudience::Diagnostics),
        "Diagnostics 只接收诊断 DTO"
    );
}

#[test]
fn prompt_recovery_rejects_unrelated_or_malformed_bodies() {
    let mut reminder = crate::system_reminder::SystemReminder {
        version: crate::system_reminder::SYSTEM_REMINDER_VERSION,
        category: crate::system_reminder::ReminderCategory::Task,
        source: crate::system_reminder::ReminderSource("cron".into()),
        kind: "triggered".into(),
        severity: crate::system_reminder::ReminderSeverity::Info,
        delivery: crate::system_reminder::ReminderDelivery::Required,
        audiences: crate::system_reminder::ReminderAudiences(vec![
            crate::system_reminder::ReminderAudience::Model,
        ]),
        body: "not a cron trigger".into(),
        summary: None,
        metadata: serde_json::json!({}),
    };
    assert!(cron_trigger_prompt(&reminder).is_none());
    reminder.body = cron_trigger_body("task-1", "do the thing") + "trailing";
    assert!(cron_trigger_prompt(&reminder).is_none());
    // 正文里出现同类标记时仍按唯一前缀/后缀解出完整指令。
    let tricky = cron_trigger_body("task-1", "say </goal-message> twice");
    reminder.body = tricky;
    assert_eq!(
        cron_trigger_prompt(&reminder).as_deref(),
        Some("say </goal-message> twice")
    );
}

#[test]
fn firing_identity_is_stable_per_firing_and_new_per_trigger() {
    let first = cron_firing_delivery_id("task-1", "2026-10-07T00:00:00+00:00");
    let retry = cron_firing_delivery_id("task-1", "2026-10-07T00:00:00+00:00");
    let next = cron_firing_delivery_id("task-1", "2026-10-07T00:05:00+00:00");
    let other_task = cron_firing_delivery_id("task-2", "2026-10-07T00:00:00+00:00");
    assert_eq!(first, retry, "同一 firing 的重试必须复用同一身份");
    assert_ne!(first, next, "新触发必须是新身份");
    assert_ne!(first, other_task);
    assert!(
        first != cron_firing_delivery_id("task-1", "2026-10-07T00:00:00+00:00!"),
        "身份由来源 + 业务事件键派生，不按正文去重"
    );
}
