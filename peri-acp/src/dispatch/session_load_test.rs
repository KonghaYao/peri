//! 测试 session_load 模块的 build_session_view_commit_payload 函数。

use super::build_session_view_commit_payload;
use peri_agent::messages::BaseMessage;

fn make_human_message(text: &str) -> BaseMessage {
    BaseMessage::human(text.to_string())
}

#[test]
fn 测试空history应返回包含空view_models的view_commit() {
    let history: Vec<BaseMessage> = vec![];
    let result = build_session_view_commit_payload("test-session", &history);
    assert!(
        result.is_some(),
        "空 history 也应返回 Some（空 view_models），以便 TUI bridge 清空旧数据"
    );
    let payload = result.unwrap();
    assert_eq!(payload["sessionId"], "test-session");
    assert_eq!(payload["event"], "view-commit");
    assert_eq!(payload["data"]["view_models"].as_array().unwrap().len(), 0);
}

#[test]
fn 测试有history时应正常转换view_models() {
    let history = vec![make_human_message("hello"), BaseMessage::ai("hi there")];
    let result = build_session_view_commit_payload("test-session", &history);
    assert!(result.is_some());
    let payload = result.unwrap();
    let vms = payload["data"]["view_models"].as_array().unwrap();
    assert!(!vms.is_empty(), "非空 history 应有 ViewModel 输出");
}
