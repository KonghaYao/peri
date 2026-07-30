use super::*;

/// 测试 strip_leaked_prepends：有原始历史时，通过 ID 匹配定位并剥离 leaked system prepends
#[test]
fn test_strip_leaked_prepends_有历史时剥离头部system消息() {
    // Arrange: 原始历史 [Human("hello"), Ai("hi")]
    let history = [BaseMessage::human("hello"), BaseMessage::ai("hi")];
    // 模拟 execute() 错误路径返回的 messages:
    // [SystemPrepend, SystemPrompt, Human("hello"), Ai("hi"), Human("new"), Ai("response")]
    let leaked_system_1 = BaseMessage::system("injected by middleware");
    let leaked_system_2 = BaseMessage::system("system prompt");
    let result_messages = vec![
        leaked_system_1,
        leaked_system_2,
        history[0].clone(),
        history[1].clone(),
        BaseMessage::human("new question"),
        BaseMessage::ai("response"),
    ];

    let cleaned = strip_leaked_prepends(&result_messages, history.first().map(|m| m.id()), false)
        .expect("完整历史应保留在结果中");

    assert_eq!(cleaned.len(), 4, "应去掉2条leaked system，剩4条");
    assert_eq!(
        cleaned[0].id(),
        history[0].id(),
        "第一条应为原始历史的第一条"
    );
    assert!(!cleaned[0].is_system(), "不应包含leaked system");
}

/// 测试 strip_leaked_prepends：原始历史为空时，剥离所有头部 system 消息
#[test]
fn test_strip_leaked_prepends_空历史时剥离头部system() {
    let history: Vec<BaseMessage> = vec![];
    let result_messages = vec![
        BaseMessage::system("injected by middleware"),
        BaseMessage::system("system prompt"),
        BaseMessage::human("new question"),
        BaseMessage::ai("response"),
    ];

    let cleaned = strip_leaked_prepends(&result_messages, history.first().map(|m| m.id()), false)
        .expect("空历史的结果应可使用");

    assert_eq!(cleaned.len(), 2, "应去掉头部两条 system，只保留 human + ai");
    assert!(!cleaned[0].is_system(), "第一条不应是system消息");
}

/// [回归测试] 取消轮的临时 transcript 未含既有历史时，不能用它覆盖内存 history。
///
/// 历史背景：取消后的下一轮 prompt 仅从 `SessionState.history` seed；若此处返回
/// 临时结果，会使当前进程丢失前文，而重启后从 ThreadStore load 又恢复前文。
#[test]
fn test_strip_leaked_prepends_未提交full_compact时拒绝替换历史() {
    let history = [BaseMessage::human("已完成的用户消息")];
    let incomplete_result = vec![
        BaseMessage::system("system prompt"),
        BaseMessage::human("本轮用户消息"),
        BaseMessage::ai("被取消前的部分输出"),
    ];

    let cleaned = strip_leaked_prepends(
        &incomplete_result,
        history.first().map(|message| message.id()),
        false,
    );

    assert!(
        cleaned.is_none(),
        "不含原历史首条消息的 partial result 不能替换 SessionState.history"
    );
}

///
/// 取消可能发生在 compact 提交后；此时 ThreadStore 已保存 excluded flags 和摘要，
/// 若拒绝这个可见快照，下一轮会 seed 已被排除的旧消息而丢失摘要上下文。
#[test]
fn test_strip_leaked_prepends_已提交full_compact时接受替换历史() {
    let history = [BaseMessage::human("已完成的用户消息")];
    let compacted_result = vec![
        BaseMessage::system("system prompt"),
        BaseMessage::human("会话摘要"),
        BaseMessage::human("本轮用户消息"),
    ];

    let cleaned = strip_leaked_prepends(
        &compacted_result,
        history.first().map(|message| message.id()),
        true,
    );

    assert_eq!(
        cleaned.expect("已提交 Full Compact 的结果必须替换 history")[0].content(),
        "会话摘要"
    );
}

/// 测试 strip_leaked_prepends：没有 leaked prepends 时正常返回
#[test]
fn test_strip_leaked_prepends_无leaked时正常返回() {
    let history = [BaseMessage::human("hello"), BaseMessage::ai("hi")];
    let result_messages = vec![
        history[0].clone(),
        history[1].clone(),
        BaseMessage::human("new question"),
    ];

    let cleaned = strip_leaked_prepends(&result_messages, history.first().map(|m| m.id()), false)
        .expect("完整历史应保留在结果中");

    assert_eq!(cleaned.len(), 3, "无leaked时应正常返回所有消息");
    assert_eq!(cleaned[0].id(), history[0].id());
}
