//! Bounded text projection of one canonical JSON row. This reader never builds a
//! complete JSON Value or a complete visible string. Only the requested UTF-8
//! window and bounded structural keys are retained. Provider replay and signatures
//! are skipped; unsupported shapes fail closed instead of exposing raw JSON.

use std::io::{self, BufRead, BufReader, Read};

const MAX_DEPTH: usize = 128;
const KEY_BYTES: usize = 128;

#[derive(Debug)]
pub(super) struct DisplayChunk {
    pub bytes: Vec<u8>,
    pub next_offset: Option<u64>,
    pub complete: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Root,
    Message,
    Content,
    Block,
    ToolCalls,
    ToolCall,
    Json,
    Skip,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Emit {
    None,
    Decoded,
    Json,
}

fn known_field(mode: Mode, key: &str) -> Option<usize> {
    let fields: &[&str] = match mode {
        Mode::Root => &[
            "type",
            "version",
            "role",
            "message",
            "reminder",
            "content",
            "tool_calls",
            "is_error",
            "id",
        ],
        Mode::Message => &[
            "role",
            "content",
            "tool_calls",
            "is_error",
            "id",
            "tool_call_id",
        ],
        Mode::Block => &[
            "type",
            "text",
            "name",
            "id",
            "input",
            "content",
            "tool_use_id",
            "is_error",
        ],
        Mode::ToolCall => &["name", "id", "arguments"],
        _ => &[],
    };
    fields.iter().position(|field| *field == key)
}

struct Window {
    offset: u64,
    position: u64,
    bytes: Vec<u8>,
    limit: usize,
}

impl Window {
    fn emit(&mut self, bytes: &[u8]) -> io::Result<()> {
        for byte in bytes {
            if self.position >= self.offset && self.bytes.len() < self.limit {
                self.bytes.push(*byte);
            }
            self.position = self
                .position
                .checked_add(1)
                .ok_or_else(|| bad("display offset overflow"))?;
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<DisplayChunk> {
        if self.offset > self.position {
            return Err(bad("display offset is beyond the entry"));
        }
        if let Err(error) = std::str::from_utf8(&self.bytes) {
            if error.error_len().is_some() {
                return Err(bad("display offset is not a UTF-8 boundary"));
            }
            self.bytes.truncate(error.valid_up_to());
        }
        let end = self.offset + self.bytes.len() as u64;
        if end < self.position && self.bytes.is_empty() {
            return Err(bad("display byte budget cannot hold a character"));
        }
        Ok(DisplayChunk {
            bytes: self.bytes,
            next_offset: (end < self.position).then_some(end),
            complete: end == self.position,
        })
    }
}

fn bad(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

pub(super) fn project(reader: impl Read, offset: u64, max_bytes: u32) -> io::Result<DisplayChunk> {
    let mut parser = Parser {
        input: BufReader::with_capacity(8 * 1024, reader),
        output: Window {
            offset,
            position: 0,
            bytes: Vec::with_capacity(max_bytes as usize),
            limit: max_bytes as usize,
        },
        embedded_tools: false,
        visibility_only: false,
        visible: None,
        last_string_bytes: 0,
        metadata: None,
    };
    parser.value(Mode::Root, 0)?;
    parser.space()?;
    if parser.peek()?.is_some() {
        return Err(bad("trailing JSON data"));
    }
    if parser.visible != Some(true) {
        return Err(unsupported("entry is not visible to this client"));
    }
    parser.output.finish()
}

/// Validate audience without materializing message strings or reminder bodies.
pub(super) fn visible(reader: impl Read) -> io::Result<bool> {
    let mut parser = Parser {
        input: BufReader::with_capacity(8 * 1024, reader),
        output: Window {
            offset: 0,
            position: 0,
            bytes: Vec::new(),
            limit: 0,
        },
        embedded_tools: false,
        visibility_only: true,
        visible: None,
        last_string_bytes: 0,
        metadata: None,
    };
    parser.value(Mode::Root, 0)?;
    parser.space()?;
    if parser.peek()?.is_some() {
        return Err(bad("trailing JSON data"));
    }
    Ok(parser.visible.unwrap_or(false))
}

struct MetadataCount {
    base_depth: usize,
    nodes: usize,
    bytes: usize,
}

struct Parser<R: Read> {
    input: BufReader<R>,
    output: Window,
    embedded_tools: bool,
    visibility_only: bool,
    visible: Option<bool>,
    last_string_bytes: usize,
    metadata: Option<MetadataCount>,
}

impl<R: Read> Parser<R> {
    fn peek(&mut self) -> io::Result<Option<u8>> {
        Ok(self.input.fill_buf()?.first().copied())
    }
    fn byte(&mut self) -> io::Result<u8> {
        let byte = self.peek()?.ok_or_else(|| bad("unexpected end of JSON"))?;
        self.input.consume(1);
        Ok(byte)
    }
    fn space(&mut self) -> io::Result<()> {
        while self
            .peek()?
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.byte()?;
        }
        Ok(())
    }
    fn expect(&mut self, byte: u8) -> io::Result<()> {
        if self.byte()? != byte {
            return Err(bad("invalid JSON delimiter"));
        }
        Ok(())
    }
    fn emit(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.visibility_only {
            Ok(())
        } else {
            self.output.emit(bytes)
        }
    }

    fn value(&mut self, mode: Mode, depth: usize) -> io::Result<()> {
        let mode = if self.visibility_only && matches!(mode, Mode::Content | Mode::ToolCalls) {
            Mode::Skip
        } else {
            mode
        };
        if let Some(count) = &mut self.metadata {
            count.nodes += 1;
            if count.nodes > peri_acp_types::system_reminder::MAX_REMINDER_METADATA_NODES
                || depth.saturating_sub(count.base_depth) + 1
                    > peri_acp_types::system_reminder::MAX_REMINDER_METADATA_DEPTH
            {
                return Err(unsupported(
                    "reminder metadata exceeds its node or depth bound",
                ));
            }
        }
        if depth > MAX_DEPTH {
            return Err(unsupported("JSON nesting exceeds the display limit"));
        }
        self.space()?;
        let byte = self.peek()?.ok_or_else(|| bad("missing JSON value"))?;
        match (mode, byte) {
            (Mode::Root | Mode::Message | Mode::Block | Mode::ToolCall, b'{') => {}
            (Mode::Root | Mode::Message | Mode::Block | Mode::ToolCall, _) => {
                return Err(unsupported("unsupported history object shape"))
            }
            (Mode::Content, b'"' | b'[') | (Mode::ToolCalls, b'[') => {}
            (Mode::Content | Mode::ToolCalls, _) => {
                return Err(unsupported("unsupported history content shape"))
            }
            _ => {}
        }
        match byte {
            b'{' => self.object(mode, depth + 1),
            b'[' => self.array(mode, depth + 1),
            b'"' => {
                let emit = if mode == Mode::Json {
                    Emit::Json
                } else if mode == Mode::Content {
                    Emit::Decoded
                } else {
                    Emit::None
                };
                self.string(emit, false)?;
                Ok(())
            }
            b't' => self.literal(b"true", mode),
            b'f' => self.literal(b"false", mode),
            b'n' => self.literal(b"null", mode),
            b'-' | b'0'..=b'9' => {
                let number = self.number()?;
                if mode == Mode::Json {
                    self.emit(&number)?;
                }
                Ok(())
            }
            _ => Err(bad("invalid JSON value")),
        }
    }

    fn literal(&mut self, literal: &[u8], mode: Mode) -> io::Result<()> {
        for byte in literal {
            self.expect(*byte)?;
        }
        if mode == Mode::Json {
            self.emit(literal)?;
        }
        self.metadata_bytes(literal.len())?;
        Ok(())
    }

    fn number(&mut self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(KEY_BYTES);
        while self
            .peek()?
            .is_some_and(|byte| matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
        {
            if bytes.len() == KEY_BYTES {
                return Err(unsupported("number exceeds canonical display limit"));
            }
            bytes.push(self.byte()?);
        }
        let number = serde_json::from_slice::<serde_json::Number>(&bytes)
            .map_err(|_| bad("invalid JSON number"))?;
        self.metadata_bytes(number.to_string().len())?;
        Ok(bytes)
    }

    fn array(&mut self, mode: Mode, depth: usize) -> io::Result<()> {
        self.expect(b'[')?;
        if mode == Mode::Json {
            self.emit(b"[")?;
        }
        self.space()?;
        if self.peek()? == Some(b']') {
            self.byte()?;
            if mode == Mode::Json {
                self.emit(b"]")?;
            }
            return Ok(());
        }
        loop {
            self.metadata_bytes(1)?;
            let member = match mode {
                Mode::Content => Mode::Block,
                Mode::ToolCalls => Mode::ToolCall,
                Mode::Json => Mode::Json,
                _ => Mode::Skip,
            };
            self.value(member, depth)?;
            self.space()?;
            match self.byte()? {
                b']' => {
                    if mode == Mode::Json {
                        self.emit(b"]")?;
                    }
                    return Ok(());
                }
                b',' => {
                    if mode == Mode::Json {
                        self.emit(b",")?;
                    }
                }
                _ => return Err(bad("invalid JSON array")),
            }
        }
    }

    fn object(&mut self, mode: Mode, depth: usize) -> io::Result<()> {
        self.expect(b'{')?;
        if mode == Mode::Json {
            self.emit(b"{")?;
        }
        if mode == Mode::ToolCall {
            self.emit(b"\n[Tool call]\n")?;
        }
        let mut block_type: Option<String> = None;
        let mut block_text_seen = false;
        let mut root_type: Option<String> = None;
        let mut root_version: Option<Vec<u8>> = None;
        let mut root_role: Option<String> = None;
        let mut root_message = false;
        let mut root_reminder = false;
        let mut root_legacy_body = false;
        let mut fields = 0usize;
        let mut seen = 0u16;
        self.space()?;
        if self.peek()? == Some(b'}') {
            self.byte()?;
            if matches!(mode, Mode::Root | Mode::Block) {
                return Err(unsupported("empty history object"));
            }
            if mode == Mode::Json {
                self.emit(b"}")?;
            }
            return Ok(());
        }
        loop {
            self.metadata_bytes(1)?;
            self.space()?;
            let key = self.string(
                if mode == Mode::Json {
                    Emit::Json
                } else {
                    Emit::None
                },
                mode != Mode::Json,
            )?;
            if let Some(field) = key.as_deref().and_then(|key| known_field(mode, key)) {
                let bit = 1 << field;
                if seen & bit != 0 {
                    return Err(bad("duplicate history field"));
                }
                seen |= bit;
            }
            self.space()?;
            self.expect(b':')?;
            self.space()?;
            if mode == Mode::Json {
                self.emit(b":")?;
            }
            if mode == Mode::Block && fields == 0 && key.as_deref() != Some("type") {
                return Err(unsupported("content block type must precede its body"));
            }
            if mode == Mode::Root
                && matches!(key.as_deref(), Some("content" | "tool_calls" | "is_error"))
            {
                root_legacy_body = true;
            }
            match (mode, key.as_deref()) {
                (Mode::Root, Some("type")) => root_type = self.string(Emit::None, true)?,
                (Mode::Root, Some("version")) => root_version = Some(self.number()?),
                (Mode::Root | Mode::Message, Some("role")) => {
                    root_role = self.string(Emit::None, true)?
                }
                (Mode::Root, Some("message")) => {
                    root_message = true;
                    self.value(Mode::Message, depth)?;
                }
                (Mode::Root, Some("reminder")) => {
                    root_reminder = true;
                    self.visible = Some(self.reminder_probe(depth)?);
                }
                (Mode::Root | Mode::Message, Some("content")) => {
                    self.emit(match root_role.as_deref() {
                        Some("user") => b"\n[User]\n",
                        Some("assistant") => b"\n[Assistant]\n",
                        Some("tool") => b"\n[Tool output]\n",
                        _ => b"\n[Content]\n",
                    })?;
                    self.value(Mode::Content, depth)?;
                }
                (Mode::Root | Mode::Message, Some("is_error"))
                    if root_role.as_deref() == Some("tool") =>
                {
                    self.emit(b"\n[Tool error]\n")?;
                    self.boolean()?;
                }
                (Mode::Root | Mode::Message, Some("tool_calls"))
                    if !self.embedded_tools && root_role.as_deref() == Some("assistant") =>
                {
                    self.value(Mode::ToolCalls, depth)?
                }
                (Mode::Block, Some("type")) => {
                    if block_type.is_some() {
                        return Err(bad("duplicate content block type"));
                    }
                    block_type = self.string(Emit::None, true)?;
                    match block_type.as_deref() {
                        Some("text" | "reasoning" | "tool_result" | "provider_state") => {}
                        Some("tool_use") => {
                            self.embedded_tools = true;
                        }
                        Some("image" | "document") => {
                            return Err(unsupported(
                                "non-text attachment requires its typed renderer",
                            ))
                        }
                        _ => return Err(unsupported("unknown history content block")),
                    }
                }
                (Mode::Block, Some(field)) => match (block_type.as_deref(), field) {
                    (Some("text"), "text") => {
                        block_text_seen = true;
                        self.emit(b"\n[Text]\n")?;
                        self.string(Emit::Decoded, false)?;
                    }
                    (Some("reasoning"), "text") => {
                        block_text_seen = true;
                        self.emit(b"\n[Reasoning]\n")?;
                        self.string(Emit::Decoded, false)?;
                    }
                    (Some("tool_use"), "name" | "id") => {
                        self.emit(b"\n[Tool]\n")?;
                        self.string(Emit::Decoded, false)?;
                    }
                    (Some("tool_use"), "input") => {
                        self.emit(b"\n[Tool input]\n")?;
                        self.value(Mode::Json, depth)?;
                    }
                    (Some("tool_result"), "content") => {
                        self.emit(b"\n[Tool output]\n")?;
                        self.value(Mode::Content, depth)?;
                    }
                    (Some("tool_result"), "tool_use_id") => {
                        self.emit(b"\n[Tool result]\n")?;
                        self.string(Emit::Decoded, false)?;
                    }
                    (Some("tool_result"), "is_error") => {
                        self.emit(b"\n[Tool error]\n")?;
                        self.boolean()?;
                    }
                    _ => self.value(Mode::Skip, depth)?,
                },
                (Mode::ToolCall, Some("name" | "id")) => {
                    self.string(Emit::Decoded, false)?;
                    self.emit(b"\n")?;
                }
                (Mode::ToolCall, Some("arguments")) => self.value(Mode::Json, depth)?,
                (Mode::Json, _) => self.value(Mode::Json, depth)?,
                _ => self.value(Mode::Skip, depth)?,
            }
            fields = fields
                .checked_add(1)
                .ok_or_else(|| bad("object field count overflow"))?;
            self.space()?;
            match self.byte()? {
                b'}' => break,
                b',' => {
                    if mode == Mode::Json {
                        self.emit(b",")?;
                    }
                }
                _ => return Err(bad("invalid JSON object")),
            }
        }
        if mode == Mode::Json {
            self.emit(b"}")?;
        }
        if mode == Mode::Block
            && matches!(block_type.as_deref(), Some("text" | "reasoning"))
            && !block_text_seen
        {
            return Err(bad("display block is missing its text"));
        }
        if mode == Mode::Message {
            let visible = matches!(root_role.as_deref(), Some("user" | "assistant" | "tool"));
            self.visible = Some(visible);
            if self.visibility_only
                && matches!(
                    root_role.as_deref(),
                    Some("user" | "assistant" | "tool" | "system")
                )
            {
                self.visible = Some(visible);
            } else if !visible {
                return Err(unsupported("message role cannot be displayed"));
            }
        }
        if mode == Mode::Root {
            let supported = match (
                root_version.as_deref(),
                root_type.as_deref(),
                root_role.as_deref(),
            ) {
                (Some(b"1"), Some("message"), None) => {
                    root_message && !root_reminder && !root_legacy_body
                }
                (Some(b"1"), Some("system_reminder"), None) => {
                    root_reminder && !root_message && !root_legacy_body
                }
                (None, None, Some("user" | "assistant" | "tool")) => {
                    self.visible = Some(true);
                    !root_message && !root_reminder
                }
                (None, None, Some("system")) if self.visibility_only => {
                    self.visible = Some(false);
                    !root_message && !root_reminder
                }
                _ => false,
            };
            if !supported {
                return Err(unsupported("unsupported persisted history envelope"));
            }
        }
        Ok(())
    }

    fn boolean(&mut self) -> io::Result<()> {
        match self.peek()? {
            Some(b't') => self.literal(b"true", Mode::Json),
            Some(b'f') => self.literal(b"false", Mode::Json),
            _ => Err(bad("expected a JSON boolean")),
        }
    }

    /// Decode one Unicode scalar at a time; even a multi-gigabyte string never
    /// becomes a String. Structural field names capture at most KEY_BYTES.
    fn string(&mut self, emit: Emit, capture: bool) -> io::Result<Option<String>> {
        self.expect(b'"')?;
        if emit == Emit::Json {
            self.emit(b"\"")?;
        }
        let mut key = Vec::with_capacity(if capture { KEY_BYTES } else { 0 });
        let mut overflow = false;
        let mut decoded_bytes = 0usize;
        loop {
            let byte = self.byte()?;
            if byte == b'"' {
                break;
            }
            let mut raw = [0u8; 12];
            raw[0] = byte;
            let (raw_len, decoded, decoded_len) = if byte == b'\\' {
                raw[1] = self.byte()?;
                let mut decoded = [0u8; 4];
                let (raw_len, len) = match raw[1] {
                    b'"' | b'\\' | b'/' => {
                        decoded[0] = raw[1];
                        (2, 1)
                    }
                    b'b' => {
                        decoded[0] = 8;
                        (2, 1)
                    }
                    b'f' => {
                        decoded[0] = 12;
                        (2, 1)
                    }
                    b'n' => {
                        decoded[0] = b'\n';
                        (2, 1)
                    }
                    b'r' => {
                        decoded[0] = b'\r';
                        (2, 1)
                    }
                    b't' => {
                        decoded[0] = b'\t';
                        (2, 1)
                    }
                    b'u' => {
                        let first = self.hex4(&mut raw[2..6])?;
                        let (scalar, raw_len) = if (0xD800..=0xDBFF).contains(&first) {
                            raw[6] = self.byte()?;
                            raw[7] = self.byte()?;
                            if &raw[6..8] != b"\\u" {
                                return Err(bad("missing low surrogate"));
                            }
                            let second = self.hex4(&mut raw[8..12])?;
                            if !(0xDC00..=0xDFFF).contains(&second) {
                                return Err(bad("invalid low surrogate"));
                            }
                            (
                                0x10000 + ((u32::from(first) - 0xD800) << 10) + u32::from(second)
                                    - 0xDC00,
                                12,
                            )
                        } else {
                            (u32::from(first), 6)
                        };
                        let character =
                            char::from_u32(scalar).ok_or_else(|| bad("invalid Unicode escape"))?;
                        let len = character.encode_utf8(&mut decoded).len();
                        (raw_len, len)
                    }
                    _ => return Err(bad("invalid JSON escape")),
                };
                (raw_len, decoded, len)
            } else {
                if byte < 0x20 {
                    return Err(bad("unescaped JSON control character"));
                }
                let len = match byte {
                    0..=0x7F => 1,
                    0xC2..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF4 => 4,
                    _ => return Err(bad("invalid UTF-8 string")),
                };
                for slot in raw.iter_mut().take(len).skip(1) {
                    *slot = self.byte()?;
                }
                std::str::from_utf8(&raw[..len]).map_err(|_| bad("invalid UTF-8 string"))?;
                let mut decoded = [0; 4];
                decoded[..len].copy_from_slice(&raw[..len]);
                (len, decoded, len)
            };
            match emit {
                Emit::Decoded => self.emit(&decoded[..decoded_len])?,
                Emit::Json => self.emit(&raw[..raw_len])?,
                Emit::None => {}
            }
            decoded_bytes = decoded_bytes
                .checked_add(decoded_len)
                .ok_or_else(|| bad("string length overflow"))?;
            self.metadata_bytes(decoded_len)?;
            if capture && !overflow {
                if key.len() + decoded_len <= KEY_BYTES {
                    key.extend_from_slice(&decoded[..decoded_len]);
                } else {
                    overflow = true;
                    key.clear();
                }
            }
        }
        if emit == Emit::Json {
            self.emit(b"\"")?;
        }
        self.last_string_bytes = decoded_bytes;
        if capture && !overflow {
            Ok(Some(
                String::from_utf8(key).map_err(|_| bad("invalid structural key"))?,
            ))
        } else {
            Ok(None)
        }
    }

    fn hex4(&mut self, raw: &mut [u8]) -> io::Result<u16> {
        let mut value = 0u16;
        for slot in raw {
            *slot = self.byte()?;
            let digit = char::from(*slot)
                .to_digit(16)
                .ok_or_else(|| bad("invalid Unicode hex digit"))?;
            value = value * 16 + digit as u16;
        }
        Ok(value)
    }

    fn metadata_bytes(&mut self, bytes: usize) -> io::Result<()> {
        if let Some(count) = &mut self.metadata {
            count.bytes = count
                .bytes
                .checked_add(bytes)
                .ok_or_else(|| bad("metadata size overflow"))?;
            if count.bytes > peri_acp_types::system_reminder::MAX_REMINDER_METADATA_BYTES {
                return Err(unsupported("reminder metadata exceeds its byte bound"));
            }
        }
        Ok(())
    }

    fn reminder_probe(&mut self, depth: usize) -> io::Result<bool> {
        use peri_acp_types::system_reminder::*;
        self.expect(b'{')?;
        let mut policy = serde_json::Map::new();
        let mut body = false;
        let mut summary = false;
        let mut metadata = false;
        loop {
            self.space()?;
            let key = self.string(Emit::None, true)?;
            self.space()?;
            self.expect(b':')?;
            self.space()?;
            match key.as_deref() {
                Some("version") => {
                    let bytes = self.number()?;
                    let value = serde_json::from_slice::<u16>(&bytes)
                        .map_err(|_| bad("invalid reminder version"))?;
                    if policy.insert("version".into(), value.into()).is_some() {
                        return Err(bad("duplicate reminder field"));
                    }
                }
                Some(field @ ("category" | "source" | "kind" | "severity" | "delivery")) => {
                    let value = self
                        .string(Emit::None, true)?
                        .ok_or_else(|| unsupported("reminder policy field is too long"))?;
                    if policy.insert(field.into(), value.into()).is_some() {
                        return Err(bad("duplicate reminder field"));
                    }
                }
                Some("audiences") => {
                    self.expect(b'[')?;
                    self.space()?;
                    let mut values = Vec::new();
                    if self.peek()? == Some(b']') {
                        self.byte()?;
                    } else {
                        loop {
                            if values.len() == 4 {
                                return Err(unsupported("too many reminder audiences"));
                            }
                            self.space()?;
                            let value = self
                                .string(Emit::None, true)?
                                .ok_or_else(|| unsupported("reminder audience is too long"))?;
                            values.push(serde_json::Value::String(value));
                            self.space()?;
                            match self.byte()? {
                                b']' => break,
                                b',' => {}
                                _ => return Err(bad("invalid reminder audiences")),
                            }
                        }
                    }
                    if policy.insert("audiences".into(), values.into()).is_some() {
                        return Err(bad("duplicate reminder field"));
                    }
                }
                Some("body") => {
                    if body {
                        return Err(bad("duplicate reminder body"));
                    }
                    body = true;
                    self.emit(b"\n[Reminder]\n")?;
                    self.string(Emit::Decoded, false)?;
                    if self.last_string_bytes > MAX_REMINDER_BODY_BYTES {
                        return Err(unsupported("reminder body exceeds its byte bound"));
                    }
                }
                Some("summary") => {
                    if summary {
                        return Err(bad("duplicate reminder summary"));
                    }
                    summary = true;
                    if self.peek()? == Some(b'n') {
                        self.literal(b"null", Mode::Skip)?;
                    } else {
                        self.emit(b"\n[Summary]\n")?;
                        self.string(Emit::Decoded, false)?;
                        if self.last_string_bytes > MAX_REMINDER_SUMMARY_BYTES {
                            return Err(unsupported("reminder summary exceeds its byte bound"));
                        }
                    }
                }
                Some("metadata") => {
                    if metadata {
                        return Err(bad("duplicate reminder metadata"));
                    }
                    metadata = true;
                    if self.peek()? != Some(b'{') {
                        return Err(unsupported("reminder metadata must be an object"));
                    }
                    self.metadata = Some(MetadataCount {
                        base_depth: depth,
                        nodes: 0,
                        bytes: 0,
                    });
                    self.value(Mode::Skip, depth)?;
                    self.metadata = None;
                }
                _ => self.value(Mode::Skip, depth)?,
            }
            self.space()?;
            match self.byte()? {
                b'}' => break,
                b',' => {}
                _ => return Err(bad("invalid reminder object")),
            }
        }
        if !body {
            return Err(bad("reminder body is missing"));
        }
        policy.insert("body".into(), "".into());
        policy.insert("metadata".into(), serde_json::json!({}));
        let dto: SystemReminder =
            serde_json::from_value(policy.into()).map_err(|_| bad("invalid reminder policy"))?;
        Ok(TrustedSystemReminderFactory::for_recovery()
            .construct(dto)
            .ok()
            .is_some_and(|reminder| {
                reminder_egress_allowed(reminder.as_reminder(), ReminderAudience::Tui)
            }))
    }
}

#[cfg(test)]
#[path = "child_display_test.rs"]
mod tests;
