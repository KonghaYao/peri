use serde::{Deserialize, Serialize};

/// 单次逻辑模型调用的归属；只用于传输关联，不进入 provider 请求体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCallContext {
    pub owner: String,
    pub execution: String,
    pub call: String,
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ModelCallParent>,
}

/// 产生子调用的因果锚；身份由调用方提供，不以 HTTP 到达顺序推断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCallParent {
    pub owner: String,
    pub execution: String,
    pub call: String,
    pub tool_call: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
}
