//! 控制器侧输入来源适配器与冻结状态（契约 §3.3/§3.6/§4）。
//!
//! 遥测不再持有输入原文，而是持有可延迟读取的来源：小输入由 langfuse-client 冻结为
//! Inline 字节，大输入经资源层落盘后只持句柄。本模块只负责两件事：
//!
//! - 把 `messages`/`tools` **已存在**的 `Arc` 借用成 [`SnapshotSource`]：不克隆正文、
//!   不重建整棵 JSON 树、不触达文件路径、不做 IO。冻结计费与落盘发生在
//!   [`LangfuseSessionLike::try_freeze`](crate::langfuse::session_like::LangfuseSessionLike::try_freeze)
//!   之后的 langfuse-client worker 中，因此桥接入口无需 IO。
//! - 记录单个 generation 的输入冻结结果 [`GenerationInput`]：冻结被拒时显式标记，
//!   禁止用旧 fallback 冒充真实请求（契约 §3.6）。
//!
//! **计量口径（`retained_bytes`）**：按**真实容量**计量，不按 JSON 长度估计——`String` 计
//! `capacity()`，`Vec` 计 `capacity() × size_of::<T>()`，`serde_json::Value` 子树递归计
//! 容器槽位与键容量；调用方做过的大 `reserve` 因此同样被计入。`write_json` 逐元素
//! `serde_json::to_value`（键序重排，理由见 [`write_sorted_messages`]）会额外产生一条
//! message/tool 的 `Value` 副本，按**最大元素**计一次——正文是流式写出的，同一时刻只有一份
//! 副本驻留，所以副本不随元素数累加。
//!
//! **显式拒收**：无法定界的动态内容（如 `BaseMessage::Tool` 的 `SafeSubagentFailure` 字段
//! 私有、容量不可观察）与计量溢出都按 [`LangfuseError::Config`] 拒收，不记 0、不截断、不
//! 估算；拒收在 Batcher 计入 `rejected_events`，上层打
//! [`REJECTED_METADATA_KEY`] 标记，因此降级可见。
//!
//! **只记账、不改数据**：本适配器只对借用到的 `Arc` 累加计数器，不复制正文、不 `reserve`、
//! 不触达文件路径、不做 IO。

use std::io;
use std::mem::size_of;
use std::sync::Arc;

use langfuse_client::snapshot::{write_stable_json, SnapshotSource};
use langfuse_client::{InputSource, LangfuseError};
use peri_agent::messages::{
    BaseMessage, ContentBlock, DocumentSource, ImageSource, MessageContent, ToolCallRequest,
};
use peri_agent::tools::{ToolDefinition, ToolExecutionEvidence};
use serde_json::Value;

/// 借用 `messages`/`tools` 的冻结来源；只克隆已存在的 `Arc`。
pub(crate) fn llm_input_source(
    messages: &Arc<Vec<BaseMessage>>,
    tools: &Arc<Vec<ToolDefinition>>,
) -> Arc<dyn SnapshotSource> {
    Arc::new(LlmInputSource {
        messages: Arc::clone(messages),
        tools: Arc::clone(tools),
    })
}

/// `{"messages":…,"tools":…}` 的借用视图。
///
/// 信封键序固定为 `messages` < `tools`（字典序，与冻结前 `json!` 路径的默认键序一致）。
struct LlmInputSource {
    messages: Arc<Vec<BaseMessage>>,
    tools: Arc<Vec<ToolDefinition>>,
}

impl SnapshotSource for LlmInputSource {
    fn retained_bytes(&self) -> Result<usize, LangfuseError> {
        // 顶层 `Vec` 只作为源树入账（整份列表从不被 `to_value` 复制）：
        // 适配器与两个 `Arc<Vec<…>>` 分配块（两个引用计数 + `Vec` 头）另计一次。
        let own = size_of::<Self>()
            + 2 * (size_of::<Vec<BaseMessage>>() + size_of::<Vec<ToolDefinition>>());
        let mut total =
            Account::slots::<BaseMessage>(self.messages.capacity(), self.messages.len())
                .source_only()
                .merge(
                    Account::slots::<ToolDefinition>(self.tools.capacity(), self.tools.len())
                        .source_only(),
                )?
                .merge(Account {
                    retained: own,
                    copy: 0,
                })?;
        // 副本按最大元素计一次（逐元素写出，同一时刻只有一份副本驻留）；元素容量逐条累加。
        let mut transient = 0usize;
        for message in self.messages.iter() {
            let element = account_message(message)?;
            total = total.merge(element.source_only())?;
            transient = transient.max(element.copy);
        }
        for tool in self.tools.iter() {
            let element = account_tool(tool)?;
            total = total.merge(element.source_only())?;
            transient = transient.max(element.copy);
        }
        total.retained.checked_add(transient).ok_or_else(overflow)
    }

    fn write_json(&self, out: &mut dyn io::Write) -> Result<(), LangfuseError> {
        write_bytes(out, b"{\"messages\":")?;
        write_sorted_messages(out, &self.messages)?;
        write_bytes(out, b",\"tools\":")?;
        write_sorted_tools(out, &self.tools)?;
        write_bytes(out, b"}")
    }
}

/// 逐元素经 `serde_json::to_value` 后按键字典序写出。
///
/// 冻结前的路径是 `json!({"messages": …, "tools": …})`，`Value::Object` 在未启用
/// `serde_json/preserve_order` 时按 BTreeMap 键序输出；直接序列化结构体会改成声明顺序，
/// 使 OTLP input 字符串字节随编译图改变（契约 §1 C1 要求字节不变、§3.4 要求与构建 feature
/// 无关）。逐元素 `to_value` + [`write_stable_json`] 固定字典序，同时峰值只与单个元素有关，
/// 不构造整棵会话树。
///
/// 键序重排必须经 `Value`：流式序列化无法在写出前比较键。该副本因此无法消除，改为在
/// [`SnapshotSource::retained_bytes`] 里按最大元素计一次（见模块「计量口径」）。
fn write_sorted_messages(
    out: &mut dyn io::Write,
    messages: &[BaseMessage],
) -> Result<(), LangfuseError> {
    write_bytes(out, b"[")?;
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            write_bytes(out, b",")?;
        }
        write_stable_json(out, &serde_json::to_value(message)?)?;
    }
    write_bytes(out, b"]")
}

/// `write_sorted_messages` 的 tools 版本（同一个键序理由，元素类型不同）。
fn write_sorted_tools(
    out: &mut dyn io::Write,
    tools: &[ToolDefinition],
) -> Result<(), LangfuseError> {
    write_bytes(out, b"[")?;
    for (index, tool) in tools.iter().enumerate() {
        if index > 0 {
            write_bytes(out, b",")?;
        }
        write_stable_json(out, &serde_json::to_value(tool)?)?;
    }
    write_bytes(out, b"]")
}

/// 写出失败只保留 io 事实：不带路径、正文或凭据（由 sink 自身状态重新分类）。
fn write_bytes(out: &mut dyn io::Write, bytes: &[u8]) -> Result<(), LangfuseError> {
    out.write_all(bytes)
        .map_err(|_| LangfuseError::Config("frozen input snapshot write failed".into()))
}

// ── 容量计量 ────────────────────────────────────────────────────────────────
//
// 口径说明（契约 §4「source sizing」）：
// - `retained`：借用到的树上真实驻留的堆字节。`String` 按 `capacity()`、`Vec` 按
//   `capacity() × size_of::<T>()`、JSON 对象按条目槽位与键 `capacity()` 计；预留容量因此
//   不会被漏计。结构体自身的 inline 字节已由持有它的 `Vec` 槽位计入，不重复计。
// - `copy`：`to_value` 中间副本的上界。副本里的 `String` 由 `String::from(&str)` 建成、
//   容量等于字节长度；JSON 数组按摊还增长上界（≤ 2×len 个 `Value` 槽）计；每个对象另按条目
//   槽位计，静态键名与 tag/枚举标签各新建一份。

/// 单个元素的两种计量；两者只在该元素被写出时同时驻留。
#[derive(Clone, Copy, Default)]
struct Account {
    /// 源树自身驻留的容量上界。
    retained: usize,
    /// `write_json` 写该元素时的 `to_value` 副本上界。
    copy: usize,
}

impl Account {
    const ZERO: Self = Self {
        retained: 0,
        copy: 0,
    };

    /// 只保留源树项（副本项由调用方按最大元素单独计一次）。
    fn source_only(self) -> Self {
        Self {
            retained: self.retained,
            copy: 0,
        }
    }

    /// 逐项 checked 累加：溢出按配置错误暴露，不回绕、不截断。
    fn merge(self, other: Self) -> Result<Self, LangfuseError> {
        Ok(Self {
            retained: self
                .retained
                .checked_add(other.retained)
                .ok_or_else(overflow)?,
            copy: self.copy.checked_add(other.copy).ok_or_else(overflow)?,
        })
    }

    /// 借用的 `String`：源侧按真实容量，副本容量等于字节长度。
    ///
    /// 入参必须是 `&String`：容量只存在于拥有者上，`&str` 看不到预留容量。
    #[allow(clippy::ptr_arg)]
    fn string(text: &String) -> Self {
        Self {
            retained: text.capacity(),
            copy: text.len(),
        }
    }

    /// 可选 `String` 字段。
    fn optional_string(text: Option<&String>) -> Self {
        text.map_or(Self::ZERO, Self::string)
    }

    /// `Vec<T>` 槽位：源侧按真实容量，副本按 JSON 数组的摊还增长上界。
    fn slots<T>(capacity: usize, len: usize) -> Self {
        Self {
            retained: capacity.saturating_mul(size_of::<T>()),
            copy: len.saturating_mul(2 * size_of::<Value>()),
        }
    }

    /// 副本里逐条新建的静态 `&'static str`（结构体键名、内部 tag 值与枚举标签）及其对象槽位；
    /// 源树对这些静态名没有任何分配，同一条目重复出现表示副本确实分配了两次。
    fn static_strs(names: &[&str]) -> Self {
        Self {
            retained: 0,
            copy: names.iter().map(|name| name.len()).sum::<usize>()
                + names.len() * (size_of::<String>() + size_of::<Value>()),
        }
    }
}

/// 计量溢出：显式拒收，不静默截断。
fn overflow() -> LangfuseError {
    LangfuseError::Config("llm input snapshot retained accounting overflows".into())
}

/// 无法定界的动态内容：容量不可观察，拒收而不是记 0 或估算。
fn unbounded() -> LangfuseError {
    LangfuseError::Config(
        "llm input snapshot contains content whose memory cannot be bounded".into(),
    )
}

/// 逐元素累加：容器自身按 `capacity` 计，元素按各自口径计。
fn account_all<T>(
    items: &[T],
    capacity: usize,
    account: impl Fn(&T) -> Result<Account, LangfuseError>,
) -> Result<Account, LangfuseError> {
    let mut total = Account::slots::<T>(capacity, items.len());
    for item in items {
        total = total.merge(account(item)?)?;
    }
    Ok(total)
}

/// `BaseMessage` 的容量上界与副本上界。
///
/// `id`（`MessageId`）内联且无堆分配，inline 字节由承载它的 `Vec` 槽位计入。
fn account_message(message: &BaseMessage) -> Result<Account, LangfuseError> {
    match message {
        BaseMessage::Human { content, .. } => {
            Account::static_strs(&["role", "user", "id", "content"])
                .merge(account_content(content)?)
        }
        BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } => Account::static_strs(&["role", "assistant", "id", "content", "tool_calls"])
            .merge(account_content(content)?)?
            .merge(account_all(
                tool_calls.as_slice(),
                tool_calls.capacity(),
                account_call,
            )?),
        BaseMessage::System { content, .. } => {
            Account::static_strs(&["role", "system", "id", "content"])
                .merge(account_content(content)?)
        }
        BaseMessage::Tool {
            tool_call_id,
            content,
            execution,
            subagent_failure,
            ..
        } => {
            if subagent_failure.is_some() {
                // `SafeSubagentFailure` 字段私有（含 `Box<SafeModelErrorDiagnostic>`），
                // 外部无法观察其容量，因此按未知上界显式拒收。
                return Err(unbounded());
            }
            Account::static_strs(&[
                "role",
                "tool",
                "id",
                "tool_call_id",
                "content",
                "is_error",
                "execution",
                "subagent_failure",
            ])
            .merge(Account::string(tool_call_id))?
            .merge(account_content(content)?)?
            .merge(account_evidence(execution.as_ref())?)
        }
    }
}

/// `MessageContent` 的三种形态（`untagged`：`Text` 是裸 JSON 字符串，没有键）。
fn account_content(content: &MessageContent) -> Result<Account, LangfuseError> {
    match content {
        MessageContent::Text(text) => Ok(Account::string(text)),
        MessageContent::Blocks(blocks) => {
            account_all(blocks.as_slice(), blocks.capacity(), account_block)
        }
        MessageContent::Raw(values) => {
            account_all(values.as_slice(), values.capacity(), account_value)
        }
    }
}

/// `ContentBlock`：键名取自其手写 `Serialize`（`type` + 各 variant 字段）。
fn account_block(block: &ContentBlock) -> Result<Account, LangfuseError> {
    match block {
        ContentBlock::Text { text } => {
            Account::static_strs(&["type", "text"]).merge(Account::string(text))
        }
        ContentBlock::Image { source } => {
            Account::static_strs(&["type", "image", "source"]).merge(account_image(source)?)
        }
        ContentBlock::Document { source, title } => {
            Account::static_strs(&["type", "document", "source", "title"])
                .merge(account_document(source)?)?
                .merge(Account::optional_string(title.as_ref()))
        }
        ContentBlock::ToolUse { id, name, input } => {
            Account::static_strs(&["type", "tool_use", "id", "name", "input"])
                .merge(Account::string(id))?
                .merge(Account::string(name))?
                .merge(account_value(input)?)
        }
        ContentBlock::ToolResult {
            id,
            tool_use_id,
            content,
            ..
        } => Account::static_strs(&[
            "type",
            "tool_result",
            "id",
            "tool_use_id",
            "content",
            "is_error",
        ])
        .merge(Account::optional_string(id.as_ref()))?
        .merge(Account::string(tool_use_id))?
        .merge(account_all(
            content.as_slice(),
            content.capacity(),
            account_block,
        )?),
        ContentBlock::Reasoning { text, signature } => {
            Account::static_strs(&["type", "reasoning", "text", "signature"])
                .merge(Account::string(text))?
                .merge(Account::optional_string(signature.as_ref()))
        }
        // 透传形态：源树与深拷贝副本都由同一棵树计量。
        ContentBlock::Unknown(value) => account_value(value),
    }
}

/// `ImageSource`（internally tagged，tag 值即 variant 名）。
fn account_image(source: &ImageSource) -> Result<Account, LangfuseError> {
    match source {
        ImageSource::Base64 { media_type, data } => {
            Account::static_strs(&["type", "base64", "media_type", "data"])
                .merge(Account::string(media_type))?
                .merge(Account::string(data))
        }
        ImageSource::Url { url } => {
            Account::static_strs(&["type", "url", "url"]).merge(Account::string(url))
        }
    }
}

/// `DocumentSource`（同一个 internally tagged 规则）。
fn account_document(source: &DocumentSource) -> Result<Account, LangfuseError> {
    match source {
        DocumentSource::Base64 { media_type, data } => {
            Account::static_strs(&["type", "base64", "media_type", "data"])
                .merge(Account::string(media_type))?
                .merge(Account::string(data))
        }
        DocumentSource::Url { url } => {
            Account::static_strs(&["type", "url", "url"]).merge(Account::string(url))
        }
        DocumentSource::Text { text } => {
            Account::static_strs(&["type", "text", "text"]).merge(Account::string(text))
        }
    }
}

/// `ToolCallRequest`（派生 `Serialize` 的三个字段）。
fn account_call(call: &ToolCallRequest) -> Result<Account, LangfuseError> {
    Account::static_strs(&["id", "name", "arguments"])
        .merge(Account::string(&call.id))?
        .merge(Account::string(&call.name))?
        .merge(account_value(&call.arguments)?)
}

/// 工具执行证据：都是可观察字段（`status` 是枚举，副本按标签字符串分配一份）。
fn account_evidence(evidence: Option<&ToolExecutionEvidence>) -> Result<Account, LangfuseError> {
    let Some(evidence) = evidence else {
        return Ok(Account::ZERO);
    };
    Account::static_strs(&[
        "status",
        evidence.status.as_str(),
        "exit_code",
        "output_ref",
        "output_truncated",
        "task_id",
    ])
    .merge(Account::optional_string(evidence.output_ref.as_ref()))?
    .merge(Account::optional_string(evidence.task_id.as_ref()))
}

/// `ToolDefinition`（`name`/`description`/`parameters`）。
fn account_tool(tool: &ToolDefinition) -> Result<Account, LangfuseError> {
    Account::static_strs(&["name", "description", "parameters"])
        .merge(Account::string(&tool.name))?
        .merge(Account::string(&tool.description))?
        .merge(account_value(&tool.parameters)?)
}

/// `serde_json::Value` 子树（与 langfuse-client `snapshot_value` 同一套容量口径）。
fn account_value(value: &Value) -> Result<Account, LangfuseError> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(Account::ZERO),
        Value::String(text) => Ok(Account::string(text)),
        Value::Array(items) => account_all(items.as_slice(), items.capacity(), account_value),
        Value::Object(map) => {
            // 对象的每个条目占一个键槽与一个值槽；源键按容量、副本键按长度各计一份。
            let slots = map.len() * (size_of::<String>() + size_of::<Value>());
            let mut total = Account {
                retained: slots,
                copy: slots,
            };
            for (key, item) in map {
                total = total
                    .merge(Account::string(key))?
                    .merge(account_value(item)?)?;
            }
            Ok(total)
        }
    }
}

/// 冻结结果被拒时的 metadata 标记键（只写稳定字段，不含正文）。
pub(crate) const REJECTED_METADATA_KEY: &str = "input_snapshot_rejected";

/// 单个 generation 的输入冻结结果。
///
/// `Frozen` 的来源在请求体到达、结束或取消时被整体移动（不等 IO）；`Rejected` 表示冻结
/// 准入被拒——batcher 已计入该 generation 拒收，此时上报不得回退到旧 fallback。
// 大变体是 inline 的来源本身（约 208 字节，无堆分配）；装箱只会把每个 generation 的一次
// 移动换成一次分配，不减少驻留。
#[allow(clippy::large_enum_variant)]
pub(crate) enum GenerationInput {
    /// 已冻结来源：小输入内存字节，大输入暂存句柄。
    Frozen(InputSource),
    /// 请求体/初始输入冻结被拒（预算、槽位或关闭）。
    Rejected,
}

impl GenerationInput {
    /// 取出可挂到事件上的来源；`Rejected` 没有可交付正文。
    pub(crate) fn into_source(self) -> Option<InputSource> {
        match self {
            Self::Frozen(source) => Some(source),
            Self::Rejected => None,
        }
    }

    /// 冻结是否被拒（决定是否打 metadata 标记）。
    pub(crate) fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected)
    }
}

/// 在 metadata 上标记该 generation 的输入冻结被拒。
pub(crate) fn mark_rejected_input(metadata: &mut serde_json::Value) {
    if let Some(object) = metadata.as_object_mut() {
        object.insert(REJECTED_METADATA_KEY.to_string(), serde_json::json!(true));
    }
}

#[cfg(test)]
#[path = "input_snapshot_test.rs"]
mod tests;
