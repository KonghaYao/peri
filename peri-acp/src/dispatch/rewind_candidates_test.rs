//! dispatch/rewind_candidates 单元测试。

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::RewindCandidate;

use super::{rewind_candidate_response, rewind_candidates, REWIND_CANDIDATE_LIMIT};

#[test]
fn test_candidates_extracts_only_user_messages() {
    let history = vec![
        BaseMessage::human("第一轮用户问题"),
        BaseMessage::ai("第一轮回答"),
        BaseMessage::human("第二轮用户问题"),
    ];

    let result = rewind_candidates(&history).unwrap();
    let messages = result["messages"].as_array().unwrap();

    assert_eq!(messages.len(), 2, "只提取 user 消息");
    // P1：最新在前（弹窗第一条 = 最近一次 user 消息 = 回退一步）
    assert_eq!(messages[0]["preview"], "第二轮用户问题");
    assert_eq!(messages[1]["preview"], "第一轮用户问题");
    assert!(
        messages[0]["id"].as_str().unwrap().len() >= 8,
        "携带服务端权威消息 id"
    );
}

#[test]
fn test_candidates_excludes_system_reminder_injection() {
    let history = vec![
        BaseMessage::human("正常用户输入"),
        BaseMessage::human("<system-reminder>后台任务完成通知</system-reminder>"),
    ];

    let result = rewind_candidates(&history).unwrap();
    let messages = result["messages"].as_array().unwrap();

    assert_eq!(messages.len(), 1, "纯系统注入消息不进入候选");
    assert_eq!(messages[0]["preview"], "正常用户输入");
}

/// Bypass 模式下首轮 user 消息末尾会被追加 `<system-reminder>` 权限通知——
/// 这类消息主体仍是用户输入，不应被过滤；且 preview 应剥离注入块，
/// 只保留用户真实输入（回填/弹窗展示的干净文本）。
#[test]
fn test_candidates_preserves_user_message_with_trailing_reminder() {
    let history = vec![BaseMessage::human(
        "请用 Write 工具创建文件\n<system-reminder>Bypass 模式：工具调用自动批准</system-reminder>",
    )];

    let result = rewind_candidates(&history).unwrap();
    let messages = result["messages"].as_array().unwrap();

    assert_eq!(
        messages.len(),
        1,
        "用户消息 + 尾部 system-reminder 应保留（非纯系统提醒）"
    );
    let preview = messages[0]["preview"].as_str().unwrap();
    assert_eq!(
        preview, "请用 Write 工具创建文件",
        "preview 应剥离 system-reminder 注入块，只含用户输入"
    );
    assert!(
        !preview.contains("<system-reminder>"),
        "preview 不得携带系统注入文本"
    );
}

/// 纯系统注入消息（含前导空白等边缘形态）不产生候选。
#[test]
fn test_candidates_drops_pure_reminder_after_strip() {
    let history = vec![BaseMessage::human(
        " \n<system-reminder>后台任务完成通知</system-reminder>\n",
    )];

    let result = rewind_candidates(&history).unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 0, "剥离后为空的注入消息不进候选");
}

#[test]
fn test_candidates_empty_history_returns_empty_list() {
    let result = rewind_candidates(&[]).unwrap();
    assert_eq!(result["messages"].as_array().unwrap().len(), 0);
}

#[test]
fn test_candidates_preview_truncated_to_200_chars() {
    let long = "x".repeat(500);
    let history = vec![BaseMessage::human(long.as_str())];

    let result = rewind_candidates(&history).unwrap();
    let preview = result["messages"][0]["preview"].as_str().unwrap();
    assert_eq!(preview.chars().count(), 200);
}

/// P1：候选按时间逆序返回——弹窗第一条 = 最近一次 user 消息 = 回退一步。
#[test]
fn test_candidates_newest_first() {
    let history = vec![
        BaseMessage::human("第一轮问题"),
        BaseMessage::ai("第一轮回答"),
        BaseMessage::human("第二轮问题"),
    ];
    let result = rewind_candidates(&history).unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["preview"], "第二轮问题", "最新在前");
    assert_eq!(messages[1]["preview"], "第一轮问题");
}

#[test]
fn test_candidates_are_bounded_to_latest_64_user_messages() {
    let history = (0..100)
        .map(|index| BaseMessage::human(format!("message-{index}")))
        .collect::<Vec<_>>();

    let result = rewind_candidates(&history).unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 64);
    assert_eq!(messages[0]["preview"], "message-99");
    assert_eq!(messages[63]["preview"], "message-36");
}

/// 尾部纯注入不占候选名额：删除纯 reminder 后仍能取满 64 条有效候选。
#[test]
fn test_trailing_pure_reminders_do_not_consume_the_limit() {
    let mut history: Vec<BaseMessage> = (0..70)
        .map(|index| BaseMessage::human(format!("valid-{index}")))
        .collect();
    history.extend((0..10).map(|index| {
        BaseMessage::human(format!("<system-reminder>notice-{index}</system-reminder>"))
    }));

    let result = rewind_candidates(&history).unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), REWIND_CANDIDATE_LIMIT);
    assert_eq!(messages[0]["preview"], "valid-69");
    assert_eq!(messages[63]["preview"], "valid-6");
}

/// 存储已经给出有界候选时，响应层仍做防御：最多 64 条、剥离注入、200 字符截断、顺序不变。
#[test]
fn test_candidate_response_is_bounded_and_cleans_previews() {
    let mut candidates: Vec<RewindCandidate> = (0..70)
        .map(|index| RewindCandidate {
            message_id: peri_acp_types::messages::MessageId::new(),
            text: format!("候选-{index}"),
        })
        .collect();
    candidates.insert(
        0,
        RewindCandidate {
            message_id: peri_acp_types::messages::MessageId::new(),
            text: format!(
                "带注入的问题\n<system-reminder>权限通知</system-reminder>\n{}",
                "x".repeat(500)
            ),
        },
    );
    candidates.insert(
        1,
        RewindCandidate {
            message_id: peri_acp_types::messages::MessageId::new(),
            text: "<system-reminder>纯注入</system-reminder>".to_owned(),
        },
    );

    let value = rewind_candidate_response(&candidates);
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages.len(), REWIND_CANDIDATE_LIMIT, "防御性上限仍生效");

    let preview = messages[0]["preview"].as_str().unwrap();
    assert!(
        !preview.contains("<system-reminder>"),
        "注入块不得进入 preview: {preview}"
    );
    assert_eq!(preview.chars().count(), 200, "200 字符截断");
    assert!(
        preview.starts_with("带注入的问题"),
        "用户文本保留在前: {preview}"
    );
    assert_eq!(
        messages[1]["preview"], "候选-0",
        "纯注入候选被丢弃，且不改变后续顺序"
    );
}
