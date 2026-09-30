use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const CONFIGURATION_METHOD: &str = "config/execute";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ConfigurationRequest {
    ReadText { path: PathBuf },
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
