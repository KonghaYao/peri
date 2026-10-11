use serde::{Deserialize, Serialize};

pub const STORE_OUTPUT_METHOD: &str = "output/store";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreOutputRequest {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredOutput {
    pub uri: String,
    pub path: String,
    pub byte_length: u64,
}
