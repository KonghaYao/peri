use super::*;
use serde_json::json;

#[test]
fn parses_supported_single_step_actions_and_preserves_set_expression() {
    let expression = Expr::BinOp {
        op: "+".into(),
        left: Box::new(Expr::StateRef { n: "$count".into() }),
        right: Box::new(Expr::Number { v: 1.0 }),
    };
    for (action, expected) in [
        (
            Action::ContinueConversation {
                message: "你好".into(),
                context: None,
            },
            GenUiAction::ToAssistant("你好".into()),
        ),
        (
            Action::Set {
                target: "$count".into(),
                value: expression.clone(),
            },
            GenUiAction::Set("$count".into(), expression),
        ),
        (
            Action::Reset {
                targets: vec!["$a".into(), "$b".into()],
            },
            GenUiAction::Reset(vec!["$a".into(), "$b".into()]),
        ),
        (
            Action::Run {
                statement_id: "job".into(),
                ref_type: peri_genui::StatementKind::Query,
            },
            GenUiAction::Run("job".into()),
        ),
    ] {
        assert_eq!(parse_action(&json!({"steps": [action]})), Ok(expected));
    }
}

#[test]
fn malformed_and_multi_step_descriptors_return_errors_without_panicking() {
    for value in [
        Value::Null,
        json!({}),
        json!({"steps": []}),
        json!({"steps": [null]}),
        json!({"steps": [{"type": "set", "target": "$n", "valueAST": {"k": "Bad"}}]}),
        json!({"steps": [{"type": "continue_conversation", "message": "a"}, {"type": "continue_conversation", "message": "b"}]}),
        json!({"steps": [{"type": "continue_conversation", "message": "a", "context": {"extra": 1}}]}),
    ] {
        assert!(parse_action(&value).is_err(), "{value}");
    }
}
