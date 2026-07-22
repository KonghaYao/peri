//! Tests
use peri_acp_types::event_data::QuestionOption;

/// 编译期断言：QuestionOption 字段可读（防止上游 DTO 变更未发现）
#[test]
fn test_question_option_struct_fields() {
    let opt = QuestionOption {
        label: "test".to_string(),
        description: "desc".to_string(),
    };
    assert_eq!(opt.label, "test");
    assert_eq!(opt.description, "desc");
}
