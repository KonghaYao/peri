//! Responses 原生记录与来源身份（W2-1a，纯协议）。
//!
//! 保存已完成 Responses output 中的 message/reasoning/function_call 项：内部按原
//! JSON 保存以便持久化往返，wire 投影只输出官方 input schema 允许的字段。来源身份
//! 用随机 nonce + 域分隔 SHA-256 摘要绑定实际 endpoint/model。
//!
//! 边界：本模块不发起 HTTP、不读写存储、不接入生产消息；「终态成功」由未来 adapter
//! 判定，构造器只接受已完成的 output。nonce/digest 不是凭据，也不提供保密或防篡改
//! 认证：持有记录的人仍可离线猜测来源，因此不落盘 URL 与凭据明文。

use std::collections::BTreeSet;
use std::fmt;

use rand::RngExt;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use url::Url;

use super::types::JsonObject;
use super::ToolCall;

/// 当前支持的原生历史版本。
pub const RESPONSES_HISTORY_VERSION: u32 = 1;

/// 摘要域前缀：固定「协议 + 版本」，避免跨协议/版本复用同一摘要。
const SOURCE_DIGEST_DOMAIN: &[u8] = b"peri-model:responses-history:v1:source";
const NONCE_LEN: usize = 16;
const DIGEST_HEX_LEN: usize = 64;

/// 原生记录/来源身份的校验失败。
///
/// 变体只携带静态字段名与版本号，不包含 item 原文、密文、URL、摘要或 nonce。
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    #[error("unsupported responses history version: {found}")]
    UnsupportedVersion { found: u32 },
    #[error("unsupported semantic item type in responses history")]
    UnsupportedItemType,
    #[error("responses history item is not a json object")]
    ItemNotObject,
    #[error("responses history item is missing required field: {field}")]
    MissingField { field: &'static str },
    #[error("responses history item has an invalid field value: {field}")]
    InvalidField { field: &'static str },
    #[error("responses history item is not completed")]
    IncompleteItem,
    #[error("duplicate item id in responses history")]
    DuplicateItemId,
    #[error("duplicate function call_id in responses history")]
    DuplicateCallId,
    #[error("function call arguments are not a json object")]
    InvalidArguments,
    #[error("responses source identity is malformed")]
    MalformedSourceIdentity,
    #[error("invalid responses endpoint for source identity")]
    InvalidEndpoint,
    #[error("invalid model name for source identity")]
    InvalidModel,
    #[error("responses history source does not match the given endpoint/model")]
    SourceMismatch,
}

/// assistant 消息的 phase（官方可选字段，仅 assistant 使用）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssistantPhase {
    Commentary,
    FinalAnswer,
}

impl AssistantPhase {
    /// wire 取值。
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Commentary => "commentary",
            Self::FinalAnswer => "final_answer",
        }
    }

    fn parse(value: &Value) -> Result<Self, HistoryError> {
        match value.as_str() {
            Some("commentary") => Ok(Self::Commentary),
            Some("final_answer") => Ok(Self::FinalAnswer),
            _ => Err(HistoryError::InvalidField { field: "phase" }),
        }
    }
}

/// 记录项类别（只允许官方 input union 中的三类语义 item）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResponsesHistoryItemKind {
    Message,
    Reasoning,
    FunctionCall,
}

#[derive(Clone, PartialEq)]
enum MessagePart {
    OutputText(String),
    Refusal(String),
}

#[derive(Clone, PartialEq)]
enum ItemPayload {
    Message {
        phase: Option<AssistantPhase>,
        content: Vec<MessagePart>,
    },
    Reasoning {
        summary: Vec<String>,
        encrypted_content: Option<String>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        /// 原始 arguments 字符串，投影时原样回放以保证字节保真。
        arguments: String,
        /// 解析后的参数对象，仅供派生 `ToolCall` 使用。
        parsed_arguments: JsonObject,
    },
}

/// 单个已完成的原生 item；未知非语义字段只随原 JSON 保存，不参与投影。
#[derive(Clone)]
pub struct ResponsesHistoryItem {
    kind: ResponsesHistoryItemKind,
    id: Option<String>,
    payload: ItemPayload,
    raw: JsonObject,
}

impl ResponsesHistoryItem {
    /// 项类别。
    pub fn kind(&self) -> ResponsesHistoryItemKind {
        self.kind
    }

    /// item id；reasoning 的 id 在官方 schema 中可选，因此可能是 `None`。
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

impl fmt::Debug for ResponsesHistoryItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 不输出原生 JSON、密文或参数。
        f.debug_struct("ResponsesHistoryItem")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Serialize for ResponsesHistoryItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.raw.serialize(serializer)
    }
}

/// 来源身份：随机 nonce + 规范化 endpoint/model 的域摘要；只持久化这两项。
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "SourceIdentityRepr")]
pub struct ResponsesSourceIdentity {
    nonce: [u8; NONCE_LEN],
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceIdentityRepr {
    nonce: String,
    digest: String,
}

impl ResponsesSourceIdentity {
    /// 采信当前实际发出的 endpoint 与 model，生成新 nonce。
    pub fn capture(endpoint: &Url, model: &str) -> Result<Self, HistoryError> {
        let endpoint = normalize_endpoint(endpoint)?;
        validate_model(model)?;
        let nonce = fresh_nonce();
        let digest = source_digest(&nonce, &endpoint, model);
        Ok(Self { nonce, digest })
    }

    /// 用存档 nonce 重算当前 endpoint/model 摘要；跨进程、跨 key 轮换稳定。
    pub fn verify(&self, endpoint: &Url, model: &str) -> Result<(), HistoryError> {
        let endpoint = normalize_endpoint(endpoint)?;
        validate_model(model)?;
        let expected = source_digest(&self.nonce, &endpoint, model);
        if constant_time_eq(expected.as_bytes(), self.digest.as_bytes()) {
            Ok(())
        } else {
            Err(HistoryError::SourceMismatch)
        }
    }

    /// 便捷布尔视图；非法 endpooint/model 仍返回错误，不会被当成「不匹配」吞掉。
    pub fn matches(&self, endpoint: &Url, model: &str) -> Result<bool, HistoryError> {
        match self.verify(endpoint, model) {
            Ok(()) => Ok(true),
            Err(HistoryError::SourceMismatch) => Ok(false),
            Err(other) => Err(other),
        }
    }
}

impl fmt::Debug for ResponsesSourceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // nonce 与 digest 均不出现在 Debug（digest 可被离线穷举比对）。
        f.write_str("ResponsesSourceIdentity(<redacted>)")
    }
}

impl Serialize for ResponsesSourceIdentity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SourceIdentityRepr {
            nonce: hex_encode(&self.nonce),
            digest: self.digest.clone(),
        }
        .serialize(serializer)
    }
}

impl TryFrom<SourceIdentityRepr> for ResponsesSourceIdentity {
    type Error = HistoryError;

    fn try_from(repr: SourceIdentityRepr) -> Result<Self, HistoryError> {
        let nonce = hex_decode(&repr.nonce).ok_or(HistoryError::MalformedSourceIdentity)?;
        let nonce: [u8; NONCE_LEN] = nonce
            .try_into()
            .map_err(|_| HistoryError::MalformedSourceIdentity)?;
        if repr.digest.len() != DIGEST_HEX_LEN || hex_decode(&repr.digest).is_none() {
            return Err(HistoryError::MalformedSourceIdentity);
        }
        Ok(Self {
            nonce,
            digest: repr.digest,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesHistoryV1Repr {
    version: u32,
    source: ResponsesSourceIdentity,
    items: Vec<Value>,
}

/// 版本化的原生历史记录；私有字段，只能经校验构造或校验反序列化得到。
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "ResponsesHistoryV1Repr")]
pub struct ResponsesHistoryV1 {
    version: u32,
    source: ResponsesSourceIdentity,
    items: Vec<ResponsesHistoryItem>,
}

impl ResponsesHistoryV1 {
    /// 从已完成的 output items 构造；重复 id/call_id、未知语义项、未完成状态一律失败。
    pub fn new(source: ResponsesSourceIdentity, items: Vec<Value>) -> Result<Self, HistoryError> {
        let mut parsed = Vec::with_capacity(items.len());
        let mut seen_ids = BTreeSet::new();
        let mut seen_call_ids = BTreeSet::new();
        for item in items {
            let item = parse_item(item)?;
            if let Some(id) = item.id.as_deref() {
                if !seen_ids.insert(id.to_owned()) {
                    return Err(HistoryError::DuplicateItemId);
                }
            }
            if let ItemPayload::FunctionCall { call_id, .. } = &item.payload {
                if !seen_call_ids.insert(call_id.clone()) {
                    return Err(HistoryError::DuplicateCallId);
                }
            }
            parsed.push(item);
        }
        Ok(Self {
            version: RESPONSES_HISTORY_VERSION,
            source,
            items: parsed,
        })
    }

    /// 记录版本。
    pub fn version(&self) -> u32 {
        self.version
    }

    /// 来源身份。
    pub fn source(&self) -> &ResponsesSourceIdentity {
        &self.source
    }

    /// 记录项数量（顺序与构造时一致）。
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// 来源校验：只接受同 endpoint/model 域。
    pub fn verify_source(&self, endpoint: &Url, model: &str) -> Result<(), HistoryError> {
        self.source.verify(endpoint, model)
    }

    /// 可见文本：按序拼接 message 的 output_text，每段只出现一次。
    pub fn visible_text(&self) -> String {
        let mut text = String::new();
        for item in &self.items {
            if let ItemPayload::Message { content, .. } = &item.payload {
                for part in content {
                    if let MessagePart::OutputText(part) = part {
                        text.push_str(part);
                    }
                }
            }
        }
        text
    }

    /// 拒答文本（与可见文本分离，不混入 `visible_text`）。
    pub fn refusals(&self) -> Vec<&str> {
        let mut refusals = Vec::new();
        for item in &self.items {
            if let ItemPayload::Message { content, .. } = &item.payload {
                for part in content {
                    if let MessagePart::Refusal(text) = part {
                        refusals.push(text.as_str());
                    }
                }
            }
        }
        refusals
    }

    /// 派生工具调用：`ToolCall::id` 取 call_id（工具结果由 adapter 按 call_id 配对），
    /// 每项 function_call 只出现一次。
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.items
            .iter()
            .filter_map(|item| match &item.payload {
                ItemPayload::FunctionCall {
                    call_id,
                    name,
                    parsed_arguments,
                    ..
                } => Some(ToolCall::new(
                    call_id.clone(),
                    name.clone(),
                    parsed_arguments.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    /// 白名单投影为官方 input items；非同域返回 `SourceMismatch` 供上层显式降级，
    /// 绝不静默回放 reasoning 密文或未知字段。
    pub fn project_input_items(
        &self,
        endpoint: &Url,
        model: &str,
    ) -> Result<Vec<JsonObject>, HistoryError> {
        self.source.verify(endpoint, model)?;
        Ok(self.items.iter().map(project_item).collect())
    }
}

impl fmt::Debug for ResponsesHistoryV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 不输出原生 items、密文或来源 url/hash/nonce。
        f.debug_struct("ResponsesHistoryV1")
            .field("version", &self.version)
            .field("source", &self.source)
            .field("item_count", &self.items.len())
            .finish_non_exhaustive()
    }
}

impl TryFrom<ResponsesHistoryV1Repr> for ResponsesHistoryV1 {
    type Error = HistoryError;

    fn try_from(repr: ResponsesHistoryV1Repr) -> Result<Self, HistoryError> {
        if repr.version != RESPONSES_HISTORY_VERSION {
            return Err(HistoryError::UnsupportedVersion {
                found: repr.version,
            });
        }
        Self::new(repr.source, repr.items)
    }
}

fn project_item(item: &ResponsesHistoryItem) -> JsonObject {
    match &item.payload {
        ItemPayload::Message { phase, content } => {
            let mut fields = Map::new();
            fields.insert("type".into(), Value::String("message".into()));
            if let Some(phase) = phase {
                fields.insert("phase".into(), Value::String(phase.as_wire().into()));
            }
            fields.insert("role".into(), Value::String("assistant".into()));
            fields.insert("status".into(), Value::String("completed".into()));
            let parts = content
                .iter()
                .map(|part| {
                    let entry = match part {
                        MessagePart::OutputText(text) => {
                            [("type", "output_text"), ("text", text.as_str())]
                        }
                        MessagePart::Refusal(text) => {
                            [("type", "refusal"), ("refusal", text.as_str())]
                        }
                    };
                    Value::Object(Map::from_iter(entry.map(|(key, value)| {
                        (key.to_owned(), Value::String(value.to_owned()))
                    })))
                })
                .collect();
            fields.insert("content".into(), Value::Array(parts));
            JsonObject::new(fields.into_iter().collect())
        }
        ItemPayload::Reasoning {
            summary,
            encrypted_content,
        } => {
            let mut fields = Map::new();
            fields.insert("type".into(), Value::String("reasoning".into()));
            if let Some(id) = &item.id {
                fields.insert("id".into(), Value::String(id.clone()));
            }
            fields.insert(
                "summary".into(),
                Value::Array(
                    summary
                        .iter()
                        .map(|text| {
                            Value::Object(Map::from_iter([
                                ("type".into(), Value::String("summary_text".into())),
                                ("text".into(), Value::String(text.clone())),
                            ]))
                        })
                        .collect(),
                ),
            );
            if let Some(encrypted) = encrypted_content {
                fields.insert("encrypted_content".into(), Value::String(encrypted.clone()));
            }
            JsonObject::new(fields.into_iter().collect())
        }
        ItemPayload::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        } => {
            let mut fields = Map::new();
            fields.insert("type".into(), Value::String("function_call".into()));
            if let Some(id) = &item.id {
                fields.insert("id".into(), Value::String(id.clone()));
            }
            fields.insert("call_id".into(), Value::String(call_id.clone()));
            fields.insert("name".into(), Value::String(name.clone()));
            fields.insert("arguments".into(), Value::String(arguments.clone()));
            JsonObject::new(fields.into_iter().collect())
        }
    }
}

fn parse_item(value: Value) -> Result<ResponsesHistoryItem, HistoryError> {
    let Value::Object(fields) = value else {
        return Err(HistoryError::ItemNotObject);
    };
    let item_type = fields
        .get("type")
        .and_then(Value::as_str)
        .ok_or(HistoryError::MissingField { field: "type" })?;
    let (kind, id, payload) = match item_type {
        "message" => {
            let id = Some(required_str(&fields, "id")?.to_owned());
            require_completed(&fields, true)?;
            if required_str(&fields, "role")? != "assistant" {
                return Err(HistoryError::InvalidField { field: "role" });
            }
            let phase = match fields.get("phase") {
                None | Some(Value::Null) => None,
                Some(value) => Some(AssistantPhase::parse(value)?),
            };
            let parts = fields
                .get("content")
                .and_then(Value::as_array)
                .ok_or(HistoryError::MissingField { field: "content" })?;
            let mut content = Vec::with_capacity(parts.len());
            for part in parts {
                let Value::Object(part) = part else {
                    return Err(HistoryError::InvalidField { field: "content" });
                };
                content.push(match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => {
                        MessagePart::OutputText(required_str(part, "text")?.to_owned())
                    }
                    Some("refusal") => {
                        MessagePart::Refusal(required_str(part, "refusal")?.to_owned())
                    }
                    _ => return Err(HistoryError::InvalidField { field: "content" }),
                });
            }
            (
                ResponsesHistoryItemKind::Message,
                id,
                ItemPayload::Message { phase, content },
            )
        }
        "reasoning" => {
            // 官方 schema 中 id 与 status 可选，缺失不强制；present 时必须已完成。
            let id = optional_str(&fields, "id")?;
            require_completed(&fields, false)?;
            let summary = match fields.get("summary") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(parts)) => {
                    let mut summary = Vec::with_capacity(parts.len());
                    for part in parts {
                        let Value::Object(part) = part else {
                            return Err(HistoryError::InvalidField { field: "summary" });
                        };
                        if part.get("type").and_then(Value::as_str) != Some("summary_text") {
                            return Err(HistoryError::InvalidField { field: "summary" });
                        }
                        summary.push(required_str(part, "text")?.to_owned());
                    }
                    summary
                }
                Some(_) => return Err(HistoryError::InvalidField { field: "summary" }),
            };
            let encrypted_content = match fields.get("encrypted_content") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
                Some(_) => {
                    return Err(HistoryError::InvalidField {
                        field: "encrypted_content",
                    });
                }
            };
            (
                ResponsesHistoryItemKind::Reasoning,
                id,
                ItemPayload::Reasoning {
                    summary,
                    encrypted_content,
                },
            )
        }
        "function_call" => {
            let id = Some(required_str(&fields, "id")?.to_owned());
            require_completed(&fields, true)?;
            let call_id = required_str(&fields, "call_id")?.to_owned();
            let name = required_str(&fields, "name")?.to_owned();
            let arguments = fields
                .get("arguments")
                .and_then(Value::as_str)
                .ok_or(HistoryError::MissingField { field: "arguments" })?
                .to_owned();
            let parsed = serde_json::from_str::<Value>(&arguments)
                .map_err(|_| HistoryError::InvalidArguments)?;
            let Value::Object(parsed) = parsed else {
                return Err(HistoryError::InvalidArguments);
            };
            (
                ResponsesHistoryItemKind::FunctionCall,
                id,
                ItemPayload::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    parsed_arguments: JsonObject::new(parsed.into_iter().collect()),
                },
            )
        }
        _ => return Err(HistoryError::UnsupportedItemType),
    };
    Ok(ResponsesHistoryItem {
        kind,
        id,
        payload,
        raw: JsonObject::new(fields.into_iter().collect()),
    })
}

fn required_str<'a>(
    fields: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, HistoryError> {
    match fields.get(field).and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        Some(_) => Err(HistoryError::InvalidField { field }),
        None => Err(HistoryError::MissingField { field }),
    }
}

fn optional_str(
    fields: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, HistoryError> {
    match fields.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(HistoryError::InvalidField { field }),
    }
}

fn require_completed(fields: &Map<String, Value>, required: bool) -> Result<(), HistoryError> {
    match fields.get("status").and_then(Value::as_str) {
        Some("completed") => Ok(()),
        Some(_) => Err(HistoryError::IncompleteItem),
        None if required => Err(HistoryError::MissingField { field: "status" }),
        None => Ok(()),
    }
}

fn normalize_endpoint(endpoint: &Url) -> Result<String, HistoryError> {
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(HistoryError::InvalidEndpoint);
    }
    // 使用 url 规范化结果：默认端口已折叠，path/query 参与比较，不同 URL 不会合并。
    Ok(endpoint.as_str().to_owned())
}

fn validate_model(model: &str) -> Result<(), HistoryError> {
    if model.trim().is_empty() {
        return Err(HistoryError::InvalidModel);
    }
    Ok(())
}

fn source_digest(nonce: &[u8; NONCE_LEN], endpoint: &str, model: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SOURCE_DIGEST_DOMAIN);
    hasher.update([0u8]);
    hash_field(&mut hasher, nonce);
    hash_field(&mut hasher, endpoint.as_bytes());
    hash_field(&mut hasher, model.as_bytes());
    hex_encode(&hasher.finalize())
}

/// 长度分隔，避免不同字段拼接后产生同一摘要。
fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn fresh_nonce() -> [u8; NONCE_LEN] {
    let mut rng = rand::rng();
    let mut nonce = [0u8; NONCE_LEN];
    nonce[..8].copy_from_slice(&rng.random_range(u64::MIN..=u64::MAX).to_be_bytes());
    nonce[8..].copy_from_slice(&rng.random_range(u64::MIN..=u64::MAX).to_be_bytes());
    nonce
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).ok())
        .collect()
}
