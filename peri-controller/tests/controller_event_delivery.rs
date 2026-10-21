//! 控制面事件投递回归（独立测试进程）。
//!
//! 这里安装**全局** subscriber，而不是线程局部 `set_default`：tracing 的
//! callsite 兴趣缓存是进程级全局的。若同一测试进程内其它线程先命中
//! `warn!` callsite，会把兴趣缓存为 `never`，使日志断言空洞通过。
//! 每个集成测试文件是独立进程，全局订阅只安装一次。

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};

use peri_acp_types::identity::{EventDeliveryClass, EventEnvelope, SessionEpoch, SessionSeq};
use peri_acp_types::session_resources::SessionResources;
use peri_controller::Controller;
use tracing::field::{Field, Visit};
use tracing::span;
use tracing::{Event, Metadata, Subscriber};

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<String>>);

impl LogCapture {
    fn clear(&self) {
        self.0.lock().expect("log buffer poisoned").clear();
    }

    fn text(&self) -> String {
        self.0.lock().expect("log buffer poisoned").clone()
    }
}

struct CaptureSubscriber {
    sink: Arc<Mutex<String>>,
}

struct RenderedFields(String);

impl Visit for RenderedFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = write!(self.0, "{}={:?}", field.name(), value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = write!(self.0, "{}={}", field.name(), value);
    }
}

impl Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = RenderedFields(String::new());
        event.record(&mut fields);
        let metadata = event.metadata();
        let mut sink = self.sink.lock().expect("log buffer poisoned");
        let _ = writeln!(
            sink,
            "{} target={} {}",
            metadata.level(),
            metadata.target(),
            fields.0
        );
    }
}

static CAPTURE: OnceLock<LogCapture> = OnceLock::new();

fn capture() -> &'static LogCapture {
    CAPTURE.get_or_init(|| {
        let capture = LogCapture::default();
        tracing::subscriber::set_global_default(CaptureSubscriber {
            sink: Arc::clone(&capture.0),
        })
        .expect("install global log capture subscriber");
        capture
    })
}

/// 回归：没有拉取式消费者时，事件只经订阅广播投递；容量用尽不得产生
/// “弹出队列未投递”告警（4.2.3 单题 13,271 条告警的根因）。
///
/// 发布条数超过 Controller 广播容量（1024），覆盖旧实现无消费者 mpsc
/// 队列填满后的路径；订阅者同步排空，隔离出“广播消费正常、仅缺拉取者”。
#[tokio::test]
async fn publishing_beyond_capacity_without_pull_consumer_logs_no_drop() {
    let capture = capture();
    // 正向校验：先确认捕获器本身生效，避免“日志为空”被当作成功证据。
    tracing::warn!(target: "peri_controller::controller", "capture probe");
    assert!(
        capture.text().contains("capture probe"),
        "日志捕获未生效: {}",
        capture.text()
    );
    capture.clear();

    let tmp = tempfile::tempdir().unwrap();
    let facade =
        peri_resources::sessions::SessionResourcesImpl::open(tmp.path().join("threads.db"))
            .await
            .unwrap();
    let controller = Controller::new(Arc::new(facade) as Arc<dyn SessionResources>);
    let mut sub = controller.subscribe();
    let env = EventEnvelope::new(
        "s1",
        SessionEpoch::initial(),
        "t1",
        "a1",
        SessionSeq::initial(),
        EventDeliveryClass::Critical,
    );

    for _ in 0..2048 {
        controller.publish(env.clone());
        assert_eq!(
            sub.try_recv().unwrap().unwrap().envelope,
            env,
            "订阅者按序收到每条事件"
        );
    }

    let logs = capture.text();
    assert!(
        !logs.contains("控制面弹出队列事件未投递"),
        "没有弹出队列消费者时不得把广播投递报告为丢失: {logs}"
    );
}
