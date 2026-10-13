//! 统一流式 OTLP 编码：唯一的映射 emitter，只替换 sink（契约 §3.4）。
//!
//! [`write_otlp`]（写入）与 [`measure_otlp`]（计量）走同一条发射路径——信封、分隔、转义与
//! 字节计数都在 [`OtlpSink`] 内完成，因此两者不可能出现两套口径。冻结 input/output 由
//! [`json_string::write_json_string`] 分块嵌入，metadata/model_parameters/usage 等
//! [`serde_json::Value`] 由 [`json_string::EscapeWriter`] 边序列化边转义，都不物化整串，
//! 也不构造整棵 `Vec<OtelSpan>`。
//!
//! 模块路径按契约 §3.4 是 crate 根下的 `crate::encoding`（`lib.rs` 的 `pub(crate) mod
//! encoding` 挂载由集成流落地）。

pub(crate) mod json_string;
pub(crate) mod writer;

pub(crate) use writer::{BudgetedWriter, ExportContext, SinkFailure, STREAM_CHUNK_BYTES};

use std::io::Write;

use serde::Serialize;

use crate::error::LangfuseError;
use crate::payload::InputSnapshot;
use crate::snapshot::{InputSource, TelemetryEvent};
use crate::stable_json::StableJson;
use crate::types::conversion::EventView;

use json_string::{write_failure, write_quoted};

/// 信封前缀：resource 属性、scope 与 `spans` 数组左边界（与 serde 基线逐字节一致）。
const ENVELOPE_HEAD: &str = concat!(
    r#"{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":""#,
    "peri-agent",
    r#""}},{"key":"service.version","value":{"stringValue":""#,
    env!("CARGO_PKG_VERSION"),
    r#""}}]},"scopeSpans":[{"scope":{"name":"langfuse-client","version":""#,
    env!("CARGO_PKG_VERSION"),
    r#""},"spans":["#,
);

/// 信封后缀：spans/scopeSpans/resourceSpans 数组与对象右边界。
const ENVELOPE_TAIL: &str = "]}]}]}";

/// span 状态：`Default` 是空对象，`Error` 带 `code=2` 与可选消息（映射规则在 `types::conversion`）。
pub(crate) enum SpanStatus<'a> {
    Default,
    Error { message: Option<&'a str> },
}

/// 待嵌入 `stringValue` 的 input/output 来源：已冻结来源，或事件 body 内的内联值。
pub(crate) enum FrozenInput<'a> {
    Value(&'a serde_json::Value),
    Source(&'a InputSource),
}

/// span 身份与时间；由 `types::conversion` 的既有校验逻辑产出，sink 只读取不解释。
///
/// 这里不复用 `types::otlp::OtelSpan`：那是物化路径的表示，编码路径不再构造它（契约 §3.4）。
pub(crate) struct SpanHead<'a> {
    pub(crate) trace_id: &'a str,
    pub(crate) span_id: &'a str,
    pub(crate) parent_span_id: Option<&'a str>,
    pub(crate) kind: i32,
    pub(crate) start_time_unix_nano: &'a str,
    pub(crate) end_time_unix_nano: &'a str,
}

/// OTLP JSON sink：唯一映射 emitter 的写入端，属性分隔状态在 span 生命周期内有效。
pub(crate) struct OtlpSink<'a> {
    out: &'a mut dyn Write,
    /// 流式嵌入冻结快照的读写块；首次需要时才分配。
    chunk: Vec<u8>,
    written: u64,
    /// 本 sink 已写出的 span 数（决定 span 之间的逗号）。
    spans: u64,
    /// 当前 span 已写出的属性数（决定属性之间的逗号）。
    attrs: u64,
}

impl<'a> OtlpSink<'a> {
    pub(crate) fn new(out: &'a mut dyn Write) -> Self {
        Self {
            out,
            chunk: Vec::new(),
            written: 0,
            spans: 0,
            attrs: 0,
        }
    }

    /// 已写出的正文字节数（解压后长度）。
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// 已写出的 span 数；批次 `event_count` 的唯一权威（SDK log/会话记录不产生 span）。
    pub(crate) fn spans(&self) -> u64 {
        self.spans
    }

    /// 写固定字节片段。
    fn put(&mut self, bytes: &[u8]) -> Result<(), LangfuseError> {
        let mut counted = Counted {
            inner: &mut *self.out,
            written: 0,
        };
        let outcome = counted.write_all(bytes).map_err(write_failure);
        self.written = self.written.saturating_add(counted.written);
        outcome
    }

    /// 写带引号的转义字符串（key/name/ID/时间/小字符串值）。
    fn put_quoted(&mut self, text: &str) -> Result<(), LangfuseError> {
        let mut counted = Counted {
            inner: &mut *self.out,
            written: 0,
        };
        let outcome = write_quoted(text, &mut counted);
        self.written = self.written.saturating_add(counted.written);
        outcome
    }

    /// 写一个**带引号的 JSON 字符串字面量**，内容是该值按紧凑 serde 输出的文本。
    ///
    /// 旧物化路径用 `value.to_string()` 再当字符串序列化，与此逐字节相同；这里边序列化边转义，
    /// 不为大值构造中间 `String`。
    fn put_json_literal<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), LangfuseError> {
        let mut counted = Counted {
            inner: &mut *self.out,
            written: 0,
        };
        let outcome = (|| {
            counted.write_all(b"\"").map_err(write_failure)?;
            serde_json::to_writer(json_string::EscapeWriter::new(&mut counted), value)
                .map_err(LangfuseError::from)?;
            counted.write_all(b"\"").map_err(write_failure)
        })();
        self.written = self.written.saturating_add(counted.written);
        outcome
    }

    /// 分块嵌入冻结 JSON 字节（inline/file 共用同一转义实现）。
    fn put_frozen(&mut self, source: &InputSource) -> Result<(), LangfuseError> {
        match source {
            InputSource::Ready(InputSnapshot::Inline(bytes)) => {
                let mut reader: &[u8] = bytes.bytes();
                self.put_streamed(&mut reader)
            }
            InputSource::Ready(InputSnapshot::Spooled(object)) => self.put_spooled_object(object),
            InputSource::Preparing(_) => Err(LangfuseError::IngestionApi(
                "OTLP encoding requires a resolved snapshot source".into(),
            )),
        }
    }

    /// 从任意 reader 分块嵌入；读块由 sink 复用，只有一个转义实现。
    fn put_streamed(&mut self, reader: &mut dyn std::io::Read) -> Result<(), LangfuseError> {
        let mut chunk = std::mem::take(&mut self.chunk);
        if chunk.is_empty() {
            chunk = vec![0u8; STREAM_CHUNK_BYTES];
        }
        let outcome = {
            let mut counted = Counted {
                inner: &mut *self.out,
                written: 0,
            };
            let outcome = json_string::write_json_string(reader, &mut counted, &mut chunk);
            self.written = self.written.saturating_add(counted.written);
            outcome
        };
        self.chunk = chunk;
        outcome
    }

    /// 借用暂存对象的交付句柄读取正文；无论成败都把同一句柄归还清理槽。
    fn put_spooled_object(
        &mut self,
        object: &crate::spool::SpoolObject,
    ) -> Result<(), LangfuseError> {
        let claim = object.take_claim().ok_or_else(|| {
            LangfuseError::IngestionApi("frozen snapshot object is held by another owner".into())
        })?;
        let outcome = match claim.open() {
            Ok(mut reader) => {
                let outcome = self.put_streamed(&mut reader);
                drop(reader);
                outcome
            }
            Err(error) => Err(LangfuseError::Spool(error)),
        };
        match (outcome, object.return_claim(claim)) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(LangfuseError::Spool(error)),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(return_error)) => {
                tracing::warn!(
                    error = %return_error,
                    "frozen snapshot claim was not returned to its cleanup slot"
                );
                Err(error)
            }
        }
    }

    /// 信封开始：resource/scope 与 `spans` 数组左边界。
    pub(crate) fn begin_envelope(&mut self) -> Result<(), LangfuseError> {
        self.put(ENVELOPE_HEAD.as_bytes())
    }

    /// 信封结束。
    pub(crate) fn end_envelope(&mut self) -> Result<(), LangfuseError> {
        self.put(ENVELOPE_TAIL.as_bytes())
    }

    /// span 头部（ID/时间来自既有校验逻辑），并打开 `attributes` 数组。
    ///
    /// 只读取身份与时间字段：`name`/`attributes`/`status` 由调用方按同一顺序写出。
    pub(crate) fn begin_span(
        &mut self,
        head: &SpanHead<'_>,
        name: &str,
    ) -> Result<(), LangfuseError> {
        if self.spans > 0 {
            self.put(b",")?;
        }
        self.spans += 1;
        self.attrs = 0;
        self.put(b"{\"traceId\":")?;
        self.put_quoted(head.trace_id)?;
        self.put(b",\"spanId\":")?;
        self.put_quoted(head.span_id)?;
        if let Some(parent) = head.parent_span_id {
            self.put(b",\"parentSpanId\":")?;
            self.put_quoted(parent)?;
        }
        self.put(b",\"name\":")?;
        self.put_quoted(name)?;
        self.put(b",\"kind\":")?;
        self.put(head.kind.to_string().as_bytes())?;
        self.put(b",\"startTimeUnixNano\":")?;
        self.put_quoted(head.start_time_unix_nano)?;
        self.put(b",\"endTimeUnixNano\":")?;
        self.put_quoted(head.end_time_unix_nano)?;
        self.put(b",\"attributes\":[")
    }

    /// span 结束：关闭 `attributes` 并写出状态。
    pub(crate) fn end_span(&mut self, status: SpanStatus<'_>) -> Result<(), LangfuseError> {
        self.put(b"],\"status\":")?;
        match status {
            SpanStatus::Default => self.put(b"{}")?,
            SpanStatus::Error { message } => {
                self.put(b"{\"code\":2")?;
                if let Some(message) = message {
                    self.put(b",\"message\":")?;
                    self.put_quoted(message)?;
                }
                self.put(b"}")?;
            }
        }
        self.put(b"}")
    }

    /// 属性前缀：分隔符、key 与 `value` 对象左边界。
    fn begin_attr(&mut self, key: &str) -> Result<(), LangfuseError> {
        if self.attrs > 0 {
            self.put(b",")?;
        }
        self.attrs += 1;
        self.put(b"{\"key\":")?;
        self.put_quoted(key)?;
        self.put(b",\"value\":{")
    }

    /// string 属性（含 key 与值的转义）。
    pub(crate) fn attr_str(&mut self, key: &str, value: &str) -> Result<(), LangfuseError> {
        self.begin_attr(key)?;
        self.put(b"\"stringValue\":")?;
        self.put_quoted(value)?;
        self.put(b"}}")
    }

    /// int 属性。
    pub(crate) fn attr_int(&mut self, key: &str, value: i64) -> Result<(), LangfuseError> {
        self.begin_attr(key)?;
        self.put(b"\"intValue\":")?;
        self.put(value.to_string().as_bytes())?;
        self.put(b"}}")
    }

    /// 内联 JSON 值属性：`stringValue` 是值的紧凑 serde 文本（与 `value.to_string()` 逐字节一致）。
    pub(crate) fn attr_json<T: Serialize + ?Sized>(
        &mut self,
        key: &str,
        value: &T,
    ) -> Result<(), LangfuseError> {
        self.begin_attr(key)?;
        self.put(b"\"stringValue\":")?;
        self.put_json_literal(value)?;
        self.put(b"}}")
    }

    /// 冻结 input/output 属性：inline 与 file 来源共用同一转义实现。
    pub(crate) fn attr_frozen(
        &mut self,
        key: &str,
        input: FrozenInput<'_>,
    ) -> Result<(), LangfuseError> {
        self.begin_attr(key)?;
        self.put(b"\"stringValue\":")?;
        match input {
            FrozenInput::Value(value) => self.put_json_literal(&StableJson(value))?,
            FrozenInput::Source(source) => self.put_frozen(source)?,
        }
        self.put(b"}}")
    }
}

/// 字节计数适配器：计数与写出复用同一条 sink 路径。
struct Counted<'a> {
    inner: &'a mut dyn Write,
    written: u64,
}

impl Write for Counted<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// 只计量不保留字节的受限 sink（`measure_otlp`）。
struct LimitedCounter {
    written: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for LimitedCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.written) {
            self.exceeded = true;
            return Err(std::io::Error::other(
                "OTLP measurement exceeds the byte limit",
            ));
        }
        self.written = self.written.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 用同一 emitter 写出一串事件视图，返回不含编码的正文长度；只服务物化回归（契约 §3.5）。
#[cfg(test)]
pub(crate) fn write_views<'a>(
    views: impl IntoIterator<Item = EventView<'a>>,
    out: &mut dyn Write,
) -> Result<u64, LangfuseError> {
    write_views_counted(views, out).map(|(written, _)| written)
}

/// 写入实现：一条路径同时给出正文字节数与实际写出的 span 数。
fn write_views_counted<'a>(
    views: impl IntoIterator<Item = EventView<'a>>,
    out: &mut dyn Write,
) -> Result<(u64, u64), LangfuseError> {
    let mut sink = OtlpSink::new(out);
    sink.begin_envelope()?;
    for view in views {
        crate::types::conversion::write_span(&view, &mut sink)?;
    }
    sink.end_envelope()?;
    Ok((sink.written(), sink.spans()))
}

/// 把事件批次写成未压缩的标准 OTLP 正文，返回解压后字节数。
///
/// 只接受已就绪的 input/output 来源：`Preparing` 由 worker 先解析，这里返回错误而不是
/// 写进一个「空内容」的 span（那会把事件塞进超出限额的批次）。
pub(crate) fn write_otlp(
    events: &[TelemetryEvent],
    out: &mut dyn Write,
) -> Result<u64, LangfuseError> {
    write_otlp_counted(events, out).map(|(written, _)| written)
}

/// 与 [`write_otlp`] 同一条发射路径，额外返回实际写出的 span 数。
///
/// `event_count` 必须排除旧规则跳过的 SDK log 与会话记录（契约 §5.2），因此由 emitter
/// 自己报数，而不是由调用方按事件数组长度推断。
pub(crate) fn write_otlp_counted(
    events: &[TelemetryEvent],
    out: &mut dyn Write,
) -> Result<(u64, u64), LangfuseError> {
    write_views_counted(events.iter().map(EventView::from_telemetry), out)
}

/// 计量与写入共用同一映射；计量侧只计数不保留字节。
pub(crate) fn measure_otlp(events: &[TelemetryEvent], limit: usize) -> Result<u64, LangfuseError> {
    let mut counter = LimitedCounter {
        written: 0,
        limit,
        exceeded: false,
    };
    match write_otlp(events, &mut counter) {
        Ok(bytes) => Ok(bytes),
        Err(_) if counter.exceeded => Err(LangfuseError::PayloadTooLarge { limit_bytes: limit }),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "stream_test.rs"]
mod tests;
