//! Stop/PostToolBatch Block 诊断日志契约：只记次数与字节数，不落 hook 正文。
//!
//! Block reason 会作为 system-reminder 反馈投给模型（既有设计），但诊断日志
//! 不得携带正文——日志可能落到客户端/磁盘，hook 自述文本不属于日志内容。

use std::sync::{Arc, Mutex};

use super::*;

/// 捕获 INFO / WARN 事件字段。
struct LevelCaptureSubscriber {
    events: Arc<Mutex<Vec<String>>>,
}

impl tracing::Subscriber for LevelCaptureSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        matches!(
            *metadata.level(),
            tracing::Level::INFO | tracing::Level::WARN
        )
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(0)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if !self.0.is_empty() {
                    self.0.push(' ');
                }
                self.0.push_str(&format!("{}={value:?}", field.name()));
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.events.lock().unwrap().push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn capture() -> (Arc<Mutex<Vec<String>>>, LevelCaptureSubscriber) {
    let events = Arc::new(Mutex::new(Vec::new()));
    (
        Arc::clone(&events),
        LevelCaptureSubscriber {
            events: Arc::clone(&events),
        },
    )
}

/// Stop Block 日志只记 count 与 reason 字节数，不记正文。
#[test]
fn stop_block_log_records_bytes_not_reason_body() {
    let (events, subscriber) = capture();
    let _guard = tracing::subscriber::set_default(subscriber);
    let block_guard = StopBlockGuard::new();

    match block_guard.on_block("secret-stop-reason-body") {
        GuardDecision::Block { count, reason } => {
            assert_eq!(count, 1);
            assert_eq!(reason, "secret-stop-reason-body", "反馈正文不受影响");
        }
        _ => panic!("首次 Block 必须返回 Block 决策"),
    }

    let text = events.lock().unwrap().join("\n");
    assert!(
        !text.contains("secret-stop-reason-body"),
        "诊断日志不得携带 hook 正文: {text}"
    );
    assert!(
        text.contains("bytes"),
        "诊断日志必须留下字节数，captured: {text}"
    );
}

/// PostToolBatch Block 走同一 guard：日志同样不得携带正文。
#[test]
fn post_tool_batch_block_log_records_bytes_not_reason_body() {
    let (events, subscriber) = capture();
    let _guard = tracing::subscriber::set_default(subscriber);
    let block_guard = StopBlockGuard::new();

    let decision = block_guard.on_block("secret-batch-reason-body");
    assert!(matches!(decision, GuardDecision::Block { count: 1, .. }));

    let text = events.lock().unwrap().join("\n");
    assert!(
        !text.contains("secret-batch-reason-body"),
        "PostToolBatch 复用的 guard 日志不得携带 hook 正文: {text}"
    );
}
