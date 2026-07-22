use super::*;

#[test]
fn test_error_display_ingestion_api() {
    let err = LangfuseError::IngestionApi("HTTP 400: bad request".into());
    let msg = format!("{}", err);
    assert!(
        msg.contains("Ingestion API returned errors"),
        "got: {}",
        msg
    );
    assert!(msg.contains("HTTP 400"), "got: {}", msg);
}

#[test]
fn test_error_display_config() {
    let err = LangfuseError::Config("test".into());
    let msg = format!("{}", err);
    assert!(msg.contains("Invalid configuration"), "got: {}", msg);
    assert!(msg.contains("test"), "got: {}", msg);
}

#[test]
fn test_error_display_channel_closed() {
    let err = LangfuseError::ChannelClosed;
    let msg = format!("{}", err);
    assert!(
        msg.contains("shut down") || msg.contains("ChannelClosed"),
        "got: {}",
        msg
    );
}

#[test]
fn test_error_display_json_serialize() {
    let err =
        LangfuseError::JsonSerialize(serde_json::from_str::<i32>("not a number").unwrap_err());
    let msg = format!("{}", err);
    assert!(msg.contains("JSON serialization failed"), "got: {}", msg);
}
