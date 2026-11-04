//! History V2 的展示投影与有界可见性判定。
//!
//! 投影规则只有这一份：own 与 inherited 都经 [`dispatch::project_payload_updates`] 与
//! [`dispatch::reminder_visible_to_client`]。oversize 条目拿不到完整 payload 时，只允许
//! 读取固定前缀判断受众，不能为了可见性读取整条正文。

use peri_acp_types::history_v2::{
    HistoryNoticeSubItem, HistorySubItem, HistoryTextSubItem, HistoryToolCallSubItem,
    HistoryToolResultSubItem,
};
use peri_acp_types::system_reminder::ReminderDelivery;

use crate::dispatch::{self, ProjectedUpdate};
use agent_client_protocol_schema::v1::{ContentBlock, ContentChunk, SessionUpdate};

/// oversize 可见性判定的最大前缀字节数；不得为判定读取整条正文。
pub(super) const VISIBILITY_PREFIX_BYTES: u32 = 1024;

/// 一个投影子项 → V2 语义子项；无法映射时记录告警并跳过。
pub(super) fn projected_sub_item(
    update: ProjectedUpdate,
    session_id: &str,
    entry_id: &str,
    index: usize,
) -> Option<HistorySubItem> {
    let sub_item_id = format!("{entry_id}:{index}");
    match update {
        ProjectedUpdate::Session(update) => {
            match *update {
                SessionUpdate::UserMessageChunk(chunk) => Some(HistorySubItem::Text(
                    text_sub_item(sub_item_id, "user", &chunk)?,
                )),
                SessionUpdate::AgentMessageChunk(chunk) => Some(HistorySubItem::Text(
                    text_sub_item(sub_item_id, "assistant", &chunk)?,
                )),
                SessionUpdate::AgentThoughtChunk(chunk) => Some(HistorySubItem::Reasoning(
                    text_sub_item(sub_item_id, "assistant", &chunk)?,
                )),
                SessionUpdate::ToolCall(tool_call) => {
                    Some(HistorySubItem::ToolCall(HistoryToolCallSubItem {
                        sub_item_id,
                        // 调用与结果跨历史行配对：键只由会话与 toolCallId 决定，
                        // 不能把各自所在 entryId 混入，否则同一调用会得到两个键。
                        invocation_key: format!("{session_id}:{}", tool_call.tool_call_id.0),
                        title: tool_call.title.clone(),
                        status: tool_status(tool_call.status),
                        input: Some(
                            tool_call
                                .raw_input
                                .clone()
                                .unwrap_or(serde_json::Value::Null),
                        ),
                        body: None,
                    }))
                }
                SessionUpdate::ToolCallUpdate(update) => {
                    let Some(status) = update.fields.status else {
                        tracing::warn!(
                            entry_id,
                            "history v2 projection skipped a tool result without status"
                        );
                        return None;
                    };
                    let output = match update.fields.raw_output {
                        Some(serde_json::Value::String(text)) => text,
                        Some(value) => serde_json::to_string(&value).ok()?,
                        None => {
                            tracing::warn!(
                                entry_id,
                                "history v2 projection skipped a tool result without output"
                            );
                            return None;
                        }
                    };
                    let tool_call_id = update.tool_call_id.0;
                    // 结果缺少调用身份时无法与任何调用配对，使用条目级稳定键显式标记，
                    // 不以结果自身的 entryId 冒充调用身份。
                    let invocation_key = if tool_call_id.is_empty() {
                        format!("tool_result:{entry_id}")
                    } else {
                        format!("{session_id}:{tool_call_id}")
                    };
                    Some(HistorySubItem::ToolResult(HistoryToolResultSubItem {
                        sub_item_id,
                        invocation_key,
                        status: tool_status(status),
                        output: Some(output),
                        body: None,
                    }))
                }
                other => {
                    tracing::warn!(
                        entry_id,
                        variant = session_update_name(&other),
                        "history v2 projection skipped an unsupported session update"
                    );
                    None
                }
            }
        }
        ProjectedUpdate::SystemReminder(reminder) => {
            if !dispatch::reminder_visible_to_client(&reminder) {
                return None;
            }
            Some(HistorySubItem::Reminder(HistoryNoticeSubItem {
                sub_item_id,
                category: enum_name(&reminder.category),
                title: reminder
                    .summary
                    .clone()
                    .unwrap_or_else(|| reminder.kind.clone()),
                text: Some(reminder.body.clone()),
                body: None,
            }))
        }
    }
}

fn text_sub_item(
    sub_item_id: String,
    role: &str,
    chunk: &ContentChunk,
) -> Option<HistoryTextSubItem> {
    let ContentBlock::Text(text) = &chunk.content else {
        tracing::warn!(
            sub_item_id,
            "history v2 projection skipped a non-text content chunk"
        );
        return None;
    };
    Some(HistoryTextSubItem {
        sub_item_id,
        role: role.to_owned(),
        text: Some(text.text.clone()),
        body: None,
    })
}

fn tool_status(status: agent_client_protocol_schema::v1::ToolCallStatus) -> String {
    enum_name(&status)
}

fn enum_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => "unknown".to_owned(),
    }
}

/// 稳定的 variant 名，仅用于告警，不进入 wire。
fn session_update_name(update: &SessionUpdate) -> &'static str {
    match update {
        SessionUpdate::UserMessageChunk(_) => "user_message_chunk",
        SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
        SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
        SessionUpdate::ToolCall(_) => "tool_call",
        SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
        _ => "other",
    }
}

/// 前缀可见性判定。
///
/// 四态中的 `Error` 由调用方以 `Err(HistoryBackendError)` 表达：读取失败必须保留
/// 原始错误语义（是否重试由错误类型决定），不得混入本枚举被当成隐藏。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrefixVisibility {
    /// 明确可见：可返回展示条目或合法正文占位。
    Visible,
    /// 明确隐藏：可以跳过并推进扫描位置。
    Hidden,
    /// 前缀内无法确定身份或受众：不得猜测，必须让整页失败。
    Undetermined,
}

/// ≤1024 字节前缀能给出的结论。
///
/// reminder 的完整出口规则还依赖 `validate()` 的 body/summary/metadata 上限，
/// 这些字段位于正文之后，前缀无法覆盖；因此前缀只能给出 `ReminderCandidate`，
/// 由调用方做一次有界完整 payload 读取并复用 [`dispatch::reminder_visible_to_client`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrefixVerdict {
    Visible,
    Hidden,
    /// reminder 前缀字段通过了受众/投递/可判定校验，仍需完整 payload 复核。
    ReminderCandidate,
    /// 前缀未覆盖所需字段或身份未知：不得猜测。
    Undetermined,
}

/// 从 ≤1024 字节前缀按 JSON 结构判定条目身份。
///
/// - `message`：`message.role` 为 user/assistant/tool 可见，system 隐藏；
/// - legacy 未包裹消息：顶层 `role` 同口径；
/// - `system_reminder`：按 canonical 字段读取 `version`/`category`/`source`/`kind`/
///   `delivery`/`audiences`，套用与出口一致的可判定规则：
///   - `audiences` 不含 `tui`、`delivery=diagnostic_only`、版本/标识/受众不合法 ⇒ 隐藏；
///   - `delivery=required|configurable` ⇒ `ReminderCandidate`（还需复核正文上限）；
///   - 字段不在前缀内 ⇒ `Undetermined`，不猜可见。
/// - 结构未知、尾部截断：`Undetermined`（fail-closed，不是隐藏）。
///
/// 只按字段读取，不做子串匹配：正文里的 `"role":"system"` 或
/// `"audiences":["tui"]` 不会影响判定。前缀尾部被切断的 UTF-8 序列表现为字符串
/// 提前结束；已经完整读到的身份/受众字段仍然生效。
pub(super) fn prefix_verdict(prefix: &[u8]) -> PrefixVerdict {
    let facts = scan_envelope(prefix);
    match facts.kind.as_deref() {
        Some("system_reminder") => reminder_verdict(&facts.reminder),
        Some("message") => message_verdict(facts.message_role.as_deref()),
        // 旧形状：类型本身声明 system 行。
        Some("system") => PrefixVerdict::Hidden,
        // 未包裹的 legacy BaseMessage：顶层 role。
        None => message_verdict(facts.top_role.as_deref()),
        Some(_) => PrefixVerdict::Undetermined,
    }
}

fn message_verdict(role: Option<&str>) -> PrefixVerdict {
    match role {
        Some("user" | "assistant" | "tool") => PrefixVerdict::Visible,
        Some("system") => PrefixVerdict::Hidden,
        _ => PrefixVerdict::Undetermined,
    }
}

/// reminder 前缀中可由 1024 字节覆盖的字段。
#[derive(Default)]
struct ReminderFacts {
    version: Option<u16>,
    category: Option<String>,
    source: Option<String>,
    kind: Option<String>,
    delivery: Option<String>,
    audiences: Option<Vec<String>>,
}

/// 与 `reminder_egress_allowed(reminder, Tui)` 的可判定部分对齐：
///
/// - 受众、版本、标识、受众唯一性、枚举值来自 canonical 字段；
/// - `delivery=diagnostic_only` 在默认出口过滤下不可投递；
/// - body/summary/metadata 上限不在前缀内，留给完整 payload 复核。
fn reminder_verdict(facts: &ReminderFacts) -> PrefixVerdict {
    let (Some(version), Some(category), Some(source), Some(kind), Some(delivery), Some(audiences)) = (
        facts.version,
        facts.category.as_deref(),
        facts.source.as_deref(),
        facts.kind.as_deref(),
        facts.delivery.as_deref(),
        facts.audiences.as_deref(),
    ) else {
        // 字段不在 1024 字节内（含 audiences 在正文之后）：不得猜测。
        return PrefixVerdict::Undetermined;
    };
    if version != peri_acp_types::system_reminder::SYSTEM_REMINDER_VERSION
        || !peri_acp_types::system_reminder::reminder_identifier_is_valid(source)
        || !peri_acp_types::system_reminder::reminder_identifier_is_valid(kind)
        || audiences.is_empty()
        || has_duplicate_audiences(audiences)
        || !known_reminder_category(category)
    {
        // `validate()` 必然失败：出口规则是隐藏，而不是 Undetermined。
        return PrefixVerdict::Hidden;
    }
    if !audiences.iter().any(|audience| audience == "tui") {
        return PrefixVerdict::Hidden;
    }
    match known_reminder_delivery(delivery) {
        Some(ReminderDelivery::Required | ReminderDelivery::Configurable) => {
            PrefixVerdict::ReminderCandidate
        }
        // diagnostic-only 不在默认出口过滤的投递范围内；未知枚举值同样无法投递。
        Some(ReminderDelivery::DiagnosticOnly) | None => PrefixVerdict::Hidden,
    }
}

fn has_duplicate_audiences(audiences: &[String]) -> bool {
    audiences
        .iter()
        .enumerate()
        .any(|(index, value)| audiences[..index].contains(value))
}

fn known_reminder_category(value: &str) -> bool {
    serde_json::from_value::<peri_acp_types::system_reminder::ReminderCategory>(
        serde_json::Value::String(value.to_owned()),
    )
    .is_ok()
}

fn known_reminder_delivery(value: &str) -> Option<ReminderDelivery> {
    serde_json::from_value::<ReminderDelivery>(serde_json::Value::String(value.to_owned())).ok()
}

#[derive(Default)]
struct EnvelopeFacts {
    kind: Option<String>,
    top_role: Option<String>,
    message_role: Option<String>,
    reminder: ReminderFacts,
}

enum ScanError {
    /// 前缀在值/字符串中途结束；已读到的字段仍然有效。
    Truncated,
    /// 形状不是合法 JSON，或目标字段类型不对。
    Invalid,
}

fn scan_envelope(prefix: &[u8]) -> EnvelopeFacts {
    let mut cursor = PrefixCursor::new(prefix);
    let mut facts = EnvelopeFacts::default();
    // 截断或尾部损坏只影响尚未读到的字段；已读到的字段保留。
    let _ = parse_top_object(&mut cursor, &mut facts);
    facts
}

fn parse_top_object(
    cursor: &mut PrefixCursor<'_>,
    facts: &mut EnvelopeFacts,
) -> Result<(), ScanError> {
    cursor.skip_ws();
    cursor.expect(b'{')?;
    loop {
        cursor.skip_ws();
        match cursor.peek() {
            Some(b'}') => {
                cursor.bump();
                return Ok(());
            }
            Some(b'"') => {}
            Some(_) => return Err(ScanError::Invalid),
            None => return Err(ScanError::Truncated),
        }
        let key = cursor.read_string()?;
        cursor.skip_ws();
        cursor.expect(b':')?;
        match key.as_str() {
            "type" => {
                cursor.skip_ws();
                facts.kind = Some(cursor.read_string()?);
            }
            "role" => {
                cursor.skip_ws();
                facts.top_role = Some(cursor.read_string()?);
            }
            "message" => {
                cursor.skip_ws();
                parse_named_object(cursor, &mut facts.message_role, "role")?;
            }
            "reminder" => {
                cursor.skip_ws();
                parse_reminder_object(cursor, &mut facts.reminder)?;
            }
            _ => cursor.skip_value()?,
        }
        cursor.skip_ws();
        match cursor.bump() {
            Some(b',') => continue,
            Some(b'}') => return Ok(()),
            Some(_) => return Err(ScanError::Invalid),
            None => return Err(ScanError::Truncated),
        }
    }
}

/// 读取对象里的单个字符串字段（`message.role`）；其余字段按 JSON 结构跳过。
fn parse_named_object(
    cursor: &mut PrefixCursor<'_>,
    target: &mut Option<String>,
    target_key: &str,
) -> Result<(), ScanError> {
    cursor.expect(b'{')?;
    loop {
        cursor.skip_ws();
        match cursor.peek() {
            Some(b'}') => {
                cursor.bump();
                return Ok(());
            }
            Some(b'"') => {}
            Some(_) => return Err(ScanError::Invalid),
            None => return Err(ScanError::Truncated),
        }
        let key = cursor.read_string()?;
        cursor.skip_ws();
        cursor.expect(b':')?;
        if key == target_key {
            cursor.skip_ws();
            *target = Some(cursor.read_string()?);
        } else {
            cursor.skip_value()?;
        }
        cursor.skip_ws();
        match cursor.bump() {
            Some(b',') => continue,
            Some(b'}') => return Ok(()),
            Some(_) => return Err(ScanError::Invalid),
            None => return Err(ScanError::Truncated),
        }
    }
}

/// 读取 reminder 的 canonical 字段；其余字段按 JSON 结构跳过。
///
/// `validate()` 还检查 body/summary/metadata 上限，这些字段位于正文之后，
/// 前缀无法覆盖，由调用方在 `ReminderCandidate` 上做有界完整读取复核。
fn parse_reminder_object(
    cursor: &mut PrefixCursor<'_>,
    facts: &mut ReminderFacts,
) -> Result<(), ScanError> {
    cursor.expect(b'{')?;
    loop {
        cursor.skip_ws();
        match cursor.peek() {
            Some(b'}') => {
                cursor.bump();
                return Ok(());
            }
            Some(b'"') => {}
            Some(_) => return Err(ScanError::Invalid),
            None => return Err(ScanError::Truncated),
        }
        let key = cursor.read_string()?;
        cursor.skip_ws();
        cursor.expect(b':')?;
        match key.as_str() {
            "version" => {
                cursor.skip_ws();
                facts.version = Some(cursor.read_u16()?);
            }
            "category" => {
                cursor.skip_ws();
                facts.category = Some(cursor.read_string()?);
            }
            "source" => {
                cursor.skip_ws();
                facts.source = Some(cursor.read_string()?);
            }
            "kind" => {
                cursor.skip_ws();
                facts.kind = Some(cursor.read_string()?);
            }
            "delivery" => {
                cursor.skip_ws();
                facts.delivery = Some(cursor.read_string()?);
            }
            "audiences" => {
                cursor.skip_ws();
                facts.audiences = Some(cursor.read_string_array()?);
            }
            _ => cursor.skip_value()?,
        }
        cursor.skip_ws();
        match cursor.bump() {
            Some(b',') => continue,
            Some(b'}') => return Ok(()),
            Some(_) => return Err(ScanError::Invalid),
            None => return Err(ScanError::Truncated),
        }
    }
}

/// 最小的前缀 JSON 游标：只做结构扫描，不要求输入是完整文档。
struct PrefixCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> PrefixCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), ScanError> {
        match self.bump() {
            Some(actual) if actual == expected => Ok(()),
            Some(_) => Err(ScanError::Invalid),
            None => Err(ScanError::Truncated),
        }
    }

    /// 读取并解码一个 JSON 字符串；`\u` 转义与代理对都按 JSON 语义处理。
    fn read_string(&mut self) -> Result<String, ScanError> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            let byte = self.bump().ok_or(ScanError::Truncated)?;
            match byte {
                b'"' => {
                    return String::from_utf8(out).map_err(|_| ScanError::Invalid);
                }
                b'\\' => self.read_escape(&mut out)?,
                // 原始控制字符在 JSON 字符串里非法；多字节 UTF-8 的续字节均 >= 0x80。
                byte if byte < 0x20 => return Err(ScanError::Invalid),
                other => out.push(other),
            }
        }
    }

    fn read_escape(&mut self, out: &mut Vec<u8>) -> Result<(), ScanError> {
        let escape = self.bump().ok_or(ScanError::Truncated)?;
        let simple = match escape {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'/' => Some(b'/'),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            _ => None,
        };
        if let Some(byte) = simple {
            out.push(byte);
            return Ok(());
        }
        if escape != b'u' {
            return Err(ScanError::Invalid);
        }
        let mut code = u32::from(self.read_hex4()?);
        if (0xD800..=0xDBFF).contains(&code) {
            self.expect(b'\\')?;
            self.expect(b'u')?;
            let low = u32::from(self.read_hex4()?);
            if !(0xDC00..=0xDFFF).contains(&low) {
                return Err(ScanError::Invalid);
            }
            code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
        } else if (0xDC00..=0xDFFF).contains(&code) {
            return Err(ScanError::Invalid);
        }
        let character = char::from_u32(code).ok_or(ScanError::Invalid)?;
        let mut buffer = [0u8; 4];
        out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        Ok(())
    }

    /// 读取非负 JSON 整数（reminder `version`）；不接受负数、小数或指数。
    fn read_u16(&mut self) -> Result<u16, ScanError> {
        let mut value = 0u32;
        let mut digits = 0usize;
        while let Some(byte) = self.peek() {
            if !byte.is_ascii_digit() {
                break;
            }
            value = value
                .saturating_mul(10)
                .saturating_add(u32::from(byte - b'0'));
            digits += 1;
            self.pos += 1;
        }
        if digits == 0 {
            return Err(ScanError::Invalid);
        }
        u16::try_from(value).map_err(|_| ScanError::Invalid)
    }

    fn read_hex4(&mut self) -> Result<u16, ScanError> {
        let mut value = 0u16;
        for _ in 0..4 {
            let byte = self.bump().ok_or(ScanError::Truncated)?;
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(ScanError::Invalid),
            };
            value = (value << 4) | u16::from(digit);
        }
        Ok(value)
    }

    fn read_string_array(&mut self) -> Result<Vec<String>, ScanError> {
        self.expect(b'[')?;
        let mut values = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.bump();
            return Ok(values);
        }
        loop {
            self.skip_ws();
            values.push(self.read_string()?);
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => return Ok(values),
                Some(_) => return Err(ScanError::Invalid),
                None => return Err(ScanError::Truncated),
            }
        }
    }

    fn skip_value(&mut self) -> Result<(), ScanError> {
        self.skip_ws();
        match self.peek() {
            Some(b'"') => self.skip_string(),
            Some(b'{') | Some(b'[') => self.skip_container(),
            Some(b't') => self.skip_literal(b"true"),
            Some(b'f') => self.skip_literal(b"false"),
            Some(b'n') => self.skip_literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.skip_number(),
            Some(_) => Err(ScanError::Invalid),
            None => Err(ScanError::Truncated),
        }
    }

    /// 跳过字符串内容（不分配、不校验 UTF-8；只保证引号与转义配对）。
    fn skip_string(&mut self) -> Result<(), ScanError> {
        self.expect(b'"')?;
        let mut escaped = false;
        loop {
            let byte = self.bump().ok_or(ScanError::Truncated)?;
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' => escaped = true,
                b'"' => return Ok(()),
                byte if byte < 0x20 => return Err(ScanError::Invalid),
                _ => {}
            }
        }
    }

    fn skip_container(&mut self) -> Result<(), ScanError> {
        let first = self.bump().ok_or(ScanError::Truncated)?;
        let mut stack = vec![match first {
            b'{' => b'}',
            b'[' => b']',
            _ => return Err(ScanError::Invalid),
        }];
        while let Some(expected) = stack.last().copied() {
            match self.bump().ok_or(ScanError::Truncated)? {
                b'"' => {
                    self.pos -= 1;
                    self.skip_string()?;
                }
                b'{' => stack.push(b'}'),
                b'[' => stack.push(b']'),
                closer @ (b'}' | b']') => {
                    if closer != expected {
                        return Err(ScanError::Invalid);
                    }
                    stack.pop();
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn skip_literal(&mut self, literal: &[u8]) -> Result<(), ScanError> {
        for expected in literal {
            match self.bump() {
                Some(actual) if actual == *expected => {}
                Some(_) => return Err(ScanError::Invalid),
                None => return Err(ScanError::Truncated),
            }
        }
        Ok(())
    }

    fn skip_number(&mut self) -> Result<(), ScanError> {
        let mut digits = 0usize;
        while let Some(byte) = self.peek() {
            match byte {
                b'0'..=b'9' => digits += 1,
                b'-' | b'+' | b'.' | b'e' | b'E' => {}
                _ => break,
            }
            self.pos += 1;
        }
        if digits == 0 {
            return Err(ScanError::Invalid);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_json(body: &str) -> Vec<u8> {
        format!(
            r#"{{"version":1,"type":"message","message":{{"role":"user","id":"00000000-0000-0000-0000-000000000000","content":"{body}"}}}}"#
        )
        .into_bytes()
    }

    fn reminder_json(delivery: &str, audiences: &str) -> Vec<u8> {
        format!(
            r#"{{"version":1,"type":"system_reminder","id":"00000000-0000-0000-0000-000000000000","reminder":{{"version":1,"category":"guidance","source":"test","kind":"notice","severity":"info","delivery":"{delivery}","audiences":[{audiences}],"body":"..."}}}}"#
        )
        .into_bytes()
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[test]
    fn every_prefix_truncation_of_a_multibyte_body_keeps_the_verdict() {
        let json = message_json(&"中文🙂".repeat(400));
        let marker = br#""role":"user""#;
        let role_done = find(&json, marker).unwrap() + marker.len();
        let mut cut_multibyte = false;
        for len in 0..=1024.min(json.len()) {
            let prefix = &json[..len];
            let verdict = prefix_verdict(prefix);
            if len >= role_done {
                assert_eq!(verdict, PrefixVerdict::Visible, "prefix len={len}");
                if std::str::from_utf8(prefix).is_err() {
                    cut_multibyte = true;
                }
            } else {
                assert_eq!(
                    verdict,
                    PrefixVerdict::Undetermined,
                    "prefix len={len} must not guess before the role is complete"
                );
            }
        }
        assert!(
            cut_multibyte,
            "the sweep must include prefixes that cut a UTF-8 sequence in half"
        );
    }

    #[test]
    fn completed_role_with_cut_tail_character_is_visible() {
        let json = message_json("x🙂");
        let emoji = "🙂".as_bytes();
        let start = find(&json, emoji).unwrap();
        let prefix = &json[..start + 2];
        assert!(std::str::from_utf8(prefix).is_err(), "cut inside the emoji");
        assert_eq!(prefix_verdict(prefix), PrefixVerdict::Visible);
    }

    #[test]
    fn audiences_outside_the_prefix_are_undetermined() {
        let id = "x".repeat(1100);
        let json = format!(
            r#"{{"version":1,"type":"system_reminder","id":"{id}","reminder":{{"version":1,"category":"guidance","source":"test","kind":"notice","severity":"info","delivery":"configurable","audiences":["tui"],"body":"..."}}}}"#
        );
        assert_eq!(
            prefix_verdict(&json.as_bytes()[..1024]),
            PrefixVerdict::Undetermined,
            "a field beyond the bounded prefix must not be guessed"
        );
        assert_eq!(
            prefix_verdict(json.as_bytes()),
            PrefixVerdict::ReminderCandidate,
            "the same document is decidable once the fields are in range"
        );
    }

    #[test]
    fn audiences_are_parsed_as_json_not_substrings() {
        let escaped = String::from_utf8(reminder_json("configurable", r#""t\u0075i""#)).unwrap();
        assert_eq!(
            prefix_verdict(escaped.as_bytes()),
            PrefixVerdict::ReminderCandidate,
            "escaped audience values decode to tui"
        );
        assert_eq!(
            prefix_verdict(&reminder_json("configurable", r#""model""#,)),
            PrefixVerdict::Hidden,
            "model-only reminders are hidden"
        );
        let body_like =
            br#"{"version":1,"type":"message","message":{"role":"user","content":"{\"role\":\"system\"}"}}"#;
        assert_eq!(
            prefix_verdict(body_like),
            PrefixVerdict::Visible,
            "field-like text inside a body string must not hide a visible role"
        );
        assert_eq!(
            prefix_verdict(br#"{"type":"message","message":{"role":"system","content":"user"}}"#),
            PrefixVerdict::Hidden
        );
    }

    #[test]
    fn reminder_delivery_and_validation_fields_match_the_egress_rule() {
        assert_eq!(
            prefix_verdict(&reminder_json("diagnostic_only", r#""model","tui""#)),
            PrefixVerdict::Hidden,
            "diagnostic-only reminders never reach the TUI"
        );
        assert_eq!(
            prefix_verdict(&reminder_json("required", r#""tui""#)),
            PrefixVerdict::ReminderCandidate
        );
        assert_eq!(
            prefix_verdict(&reminder_json("configurable", r#""tui""#)),
            PrefixVerdict::ReminderCandidate
        );
        assert_eq!(
            prefix_verdict(&reminder_json("configurable", "")),
            PrefixVerdict::Hidden,
            "empty audiences fail validate()"
        );
        assert_eq!(
            prefix_verdict(&reminder_json("configurable", r#""tui","tui""#)),
            PrefixVerdict::Hidden,
            "duplicate audiences fail validate()"
        );
        let invalid_source = String::from_utf8(reminder_json("configurable", r#""tui""#))
            .unwrap()
            .replace("\"source\":\"test\"", "\"source\":\"Bad Source\"");
        assert_eq!(
            prefix_verdict(invalid_source.as_bytes()),
            PrefixVerdict::Hidden,
            "invalid identifiers fail validate()"
        );
        let unknown_category = String::from_utf8(reminder_json("configurable", r#""tui""#))
            .unwrap()
            .replace("\"category\":\"guidance\"", "\"category\":\"nonsense\"");
        assert_eq!(
            prefix_verdict(unknown_category.as_bytes()),
            PrefixVerdict::Hidden,
            "unknown category values cannot deserialize"
        );
        assert_eq!(
            prefix_verdict(
                br#"{"type":"system_reminder","id":"x","reminder":{"audiences":["tui"]}}"#
            ),
            PrefixVerdict::Undetermined,
            "missing version/category/source/kind/delivery must not guess"
        );
    }

    #[test]
    fn unknown_or_missing_identity_is_undetermined() {
        assert_eq!(
            prefix_verdict(br#"{"type":"message","message":{"content":"no role"}}"#),
            PrefixVerdict::Undetermined
        );
        assert_eq!(
            prefix_verdict(br#"{"version":1,"type":"system_reminder","id":"x""#),
            PrefixVerdict::Undetermined,
            "a reminder truncated before its fields is undecidable"
        );
        assert_eq!(
            prefix_verdict(br#"{"type":"unknown","role":"user"}"#),
            PrefixVerdict::Undetermined
        );
        assert_eq!(
            prefix_verdict(&[0xff, 0xfe, 0xfd]),
            PrefixVerdict::Undetermined
        );
    }
}
