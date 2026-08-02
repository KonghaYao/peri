//! rewind_candidates 单元测试——响应解析纯函数。

use serde_json::json;

use super::parse_candidates_response;
use crate::kit::atoms::{REWIND_PREVIEW, init_atoms};
use serial_test::serial;

#[test]
fn test_parse_candidates_response_extracts_messages() {
    let resp = json!({
        "messages": [
            { "id": "m1", "preview": "第一轮问题" },
            { "id": "m2", "preview": "第二轮问题" },
        ]
    });

    let candidates = parse_candidates_response(&resp).unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].id, "m1");
    assert_eq!(candidates[0].preview, "第一轮问题");
    assert_eq!(candidates[1].id, "m2");
}

#[test]
fn test_parse_candidates_response_empty_ok() {
    let resp = json!({ "messages": [] });
    let candidates = parse_candidates_response(&resp).unwrap();
    assert!(candidates.is_empty());
}

#[test]
fn test_parse_candidates_response_malformed_returns_err() {
    let resp = json!({ "unexpected": 1 });
    assert!(parse_candidates_response(&resp).is_err());
}

#[test]
#[serial]
fn test_apply_candidates_writes_preview_atom() {
    init_atoms();
    let candidates = vec![crate::kit::rewind_candidates::RewindCandidate {
        id: "m1".into(),
        preview: "问题".into(),
    }];
    crate::kit::rewind_candidates::apply_candidates(&candidates);

    let preview = REWIND_PREVIEW.state().read().clone();
    let preview = preview.expect("候选应写入 REWIND_PREVIEW atom");
    assert_eq!(preview.messages.len(), 1);
    assert_eq!(preview.messages[0].id, "m1");
    assert_eq!(preview.messages[0].role, "user");
    assert_eq!(preview.messages[0].preview, "问题");
    assert!(preview.files.is_empty(), "候选查询不携带文件信息");
}
