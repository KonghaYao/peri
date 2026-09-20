use agent_client_protocol::schema::v1::{
    RequestPermissionOutcome, RequestPermissionResponse, SelectedPermissionOutcome,
};
use agent_client_protocol_schema::v1::{CreateElicitationResponse, ElicitationAction};
use peri_acp_types::interaction::UnansweredCause;
use serde_json::Value;

/// Build a schema-valid permission response selecting the one-shot allow option.
pub fn permission_selected_allow_once_response() -> Value {
    serde_json::to_value(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("allow_once")),
    ))
    .expect("typed permission response must serialize")
}

/// Build a schema-valid cancellation response for a permission request.
pub fn permission_cancelled_response() -> Value {
    serde_json::to_value(RequestPermissionResponse::new(
        RequestPermissionOutcome::Cancelled,
    ))
    .expect("typed permission response must serialize")
}

/// Build a schema-valid cancellation response for an elicitation request.
pub fn elicitation_cancel_response() -> Value {
    serde_json::to_value(CreateElicitationResponse::new(ElicitationAction::Cancel))
        .expect("typed elicitation response must serialize")
}

/// Build the cancellation response of a client that cannot answer at all:
/// `cancel` plus a `_meta` cause declaring why. The host broker turns it into
/// `InteractionResponse::Unanswered`, so the tool reports "no user can answer"
/// instead of a fabricated empty answer.
pub fn elicitation_unanswered_response(cause: UnansweredCause) -> Value {
    let response = CreateElicitationResponse::new(ElicitationAction::Cancel)
        .meta(serde_json::Map::from_iter([cause.meta_entry()]));
    serde_json::to_value(response).expect("typed elicitation response must serialize")
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{RequestPermissionOutcome, RequestPermissionResponse};
    use agent_client_protocol_schema::v1::{CreateElicitationResponse, ElicitationAction};

    use super::*;

    #[test]
    fn test_permission_selected_response_matches_sdk_schema() {
        let response: RequestPermissionResponse =
            serde_json::from_value(permission_selected_allow_once_response()).unwrap();
        let RequestPermissionOutcome::Selected(selected) = response.outcome else {
            panic!("permission response 应为 Selected")
        };
        assert_eq!(selected.option_id.0.as_ref(), "allow_once");
    }

    #[test]
    fn test_permission_cancelled_response_matches_sdk_schema() {
        let response: RequestPermissionResponse =
            serde_json::from_value(permission_cancelled_response()).unwrap();
        assert!(matches!(
            response.outcome,
            RequestPermissionOutcome::Cancelled
        ));
    }

    #[test]
    fn test_elicitation_cancel_response_matches_sdk_schema() {
        let response: CreateElicitationResponse =
            serde_json::from_value(elicitation_cancel_response()).unwrap();
        assert!(matches!(response.action, ElicitationAction::Cancel));
    }

    /// Wire 字面量：只有 `action=cancel` + `_meta.peri.elicitationUnanswered` 同时
    /// 成立，宿主 broker 才产出 `Unanswered`；键名或取值漂移会让 `-p` 退回空答案。
    #[test]
    fn test_elicitation_unanswered_response_declares_cause() {
        let value = elicitation_unanswered_response(UnansweredCause::NonInteractiveClient);
        assert_eq!(
            value,
            serde_json::json!({
                "action": "cancel",
                "_meta": {
                    (peri_acp_types::interaction::ELICITATION_UNANSWERED_META_KEY):
                        "non_interactive_client"
                }
            }),
            "非交互声明必须落在 wire 字面键值上"
        );
        let response: CreateElicitationResponse = serde_json::from_value(value.clone()).unwrap();
        assert!(matches!(response.action, ElicitationAction::Cancel));
        assert_eq!(
            UnansweredCause::from_meta(response.meta.as_ref()),
            Some(UnansweredCause::NonInteractiveClient),
            "_meta 必须能被 broker 读回：{value}"
        );
    }
}
