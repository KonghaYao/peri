use agent_client_protocol::schema::v1::{
    RequestPermissionOutcome, RequestPermissionResponse, SelectedPermissionOutcome,
};
use agent_client_protocol_schema::v1::{CreateElicitationResponse, ElicitationAction};
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
}
