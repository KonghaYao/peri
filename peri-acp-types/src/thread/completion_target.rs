//! 子任务当前结果收件人；复用 thread config，不改写历史父子关系。

use serde_json::{Map, Value};

use super::ThreadId;

const KEY: &str = "completion_target";

#[derive(Debug, thiserror::Error)]
pub enum CompletionTargetError {
    #[error("child completion target is unavailable; stop and explicitly resume the child to establish a delivery route")]
    MissingTarget,
    #[error("invalid thread config: {0}")]
    InvalidConfig(#[from] serde_json::Error),
    #[error("thread config must be a JSON object")]
    NotAnObject,
}

/// 读取明确保存的收件人；旧数据或不透明回调不能回退到历史父节点。
pub fn completion_target(config: Option<&str>) -> Result<ThreadId, CompletionTargetError> {
    let config = config_object(config)?;
    config
        .get(KEY)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or(CompletionTargetError::MissingTarget)
}

/// 只更新必要的收件配置；None 明确表示自定义回调没有可恢复的目标。
pub fn with_completion_target(
    config: Option<&str>,
    target: Option<&str>,
) -> Result<String, CompletionTargetError> {
    let mut config = config_object(config)?;
    config.insert(
        KEY.to_owned(),
        target.map_or(Value::Null, |id| Value::String(id.to_owned())),
    );
    serde_json::to_string(&config).map_err(CompletionTargetError::from)
}

fn config_object(config: Option<&str>) -> Result<Map<String, Value>, CompletionTargetError> {
    match config {
        None => Ok(Map::new()),
        Some(text) => match serde_json::from_str(text)? {
            Value::Object(object) => Ok(object),
            _ => Err(CompletionTargetError::NotAnObject),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_overwrite_preserves_other_config_and_never_guesses() {
        let first = with_completion_target(Some(r#"{"model":"saved"}"#), Some("a")).unwrap();
        let second = with_completion_target(Some(&first), Some("b")).unwrap();
        assert_eq!(completion_target(Some(&second)).unwrap(), "b");
        assert_eq!(config_object(Some(&second)).unwrap()["model"], "saved");
        let opaque = with_completion_target(Some(&second), None).unwrap();
        assert!(completion_target(Some(&opaque)).is_err());
        assert!(completion_target(None).is_err());
        assert!(with_completion_target(Some("broken"), Some("b")).is_err());
    }
}
