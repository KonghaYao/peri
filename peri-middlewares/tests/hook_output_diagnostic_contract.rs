//! Hook 输出诊断契约：无效输出必须被拒绝，且诊断不携带 hook 正文。
//!
//! 单独成测试二进制：`tracing` 的 callsite interest 是进程级多订阅者 AND，
//! 与其它并行测试的 level 过滤订阅共存时，断言会依赖测试调度。

use std::sync::{Arc, Mutex};

use peri_middlewares::hooks::{parse_command_hook_output, HookAction, HookEvent};

struct ErrorCapture(Arc<Mutex<Vec<String>>>);

impl tracing::Subscriber for ErrorCapture {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::ERROR
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push_str(&format!("{}={value:?} ", field.name()));
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }
}

#[test]
fn invalid_output_is_diagnosed_without_body() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(ErrorCapture(Arc::clone(&events)));

    let action = parse_command_hook_output(
        &HookEvent::PermissionRequest,
        r#"{"hook_specific_output":{"hookEventName":"secret-unknown-event","secret-body":"deny"}}"#,
    );
    assert!(matches!(action, HookAction::Block { .. }));

    let output = events.lock().unwrap().join("\n");
    assert!(output.contains("PermissionRequest"));
    assert!(output.contains("invalid_command_json"));
    assert!(!output.contains("secret-unknown-event"));
    assert!(!output.contains("secret-body"));
}
