use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const CONFIGURATION_METHOD: &str = "config/execute";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ConfigurationRequest {
    ReadText { path: PathBuf },
    ReadEnvironment { name: String },
    WriteTextAtomic { path: PathBuf, content: String },
    Exists { path: PathBuf },
    SameFile { first: PathBuf, second: PathBuf },
    Paths,
    SetGlobalPath { path: Option<PathBuf> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigurationPaths {
    pub home: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub global_settings: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ConfigurationValue {
    Text(String),
    Environment(Option<String>),
    Bool(bool),
    Paths(ConfigurationPaths),
    Written,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationErrorKind {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    InvalidInput,
    InvalidData,
    TimedOut,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigurationFailure {
    pub kind: ConfigurationErrorKind,
    pub message: String,
}

pub type ConfigurationResponse = Result<ConfigurationValue, ConfigurationFailure>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_environment_request_round_trips_without_a_local_fallback_contract() {
        let request = ConfigurationRequest::ReadEnvironment {
            name: "PERI_MCP_CACHE".into(),
        };
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({"operation": "read_environment", "name": "PERI_MCP_CACHE"})
        );
        let decoded: ConfigurationRequest = serde_json::from_value(encoded).unwrap();
        assert!(
            matches!(decoded, ConfigurationRequest::ReadEnvironment { name } if name == "PERI_MCP_CACHE")
        );
    }

    #[test]
    fn environment_response_preserves_absent_empty_and_explicit_values() {
        for value in [None, Some(String::new()), Some("false".into())] {
            let response: ConfigurationResponse =
                Ok(ConfigurationValue::Environment(value.clone()));
            let encoded = serde_json::to_value(response).unwrap();
            let decoded: ConfigurationResponse = serde_json::from_value(encoded).unwrap();
            match decoded.unwrap() {
                ConfigurationValue::Environment(actual) => assert_eq!(actual, value),
                _ => panic!("environment response changed its type"),
            }
        }
    }
}
