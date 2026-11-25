//! 帧解码槽位的时序契约：只保留最新帧、收尾解完在途帧、卡住的解码不拖住收尾。

use super::*;
use crate::kit::apps::state::{APPS_STATE, AppEntry, AppView, AppsState, ViewPhase};
use std::time::Instant;

const RESOURCE_URI: &str = "ui://get-time/mcp-app.html";

fn seed_view(view_id: &str) {
    APPS_STATE.state().set(AppsState::default());
    state::insert_view(AppView::new(
        view_id,
        AppEntry {
            session_id: "session-1".to_string(),
            app_id: "app-1".to_string(),
            server_id: "demo".to_string(),
            resource_uri: RESOURCE_URI.to_string(),
            instantiating_tool: "get-time".to_string(),
            tool_call_id: Some("call-1".to_string()),
        },
    ));
}

fn applied_seq(view_id: &str) -> Option<u64> {
    APPS_STATE
        .state()
        .read()
        .view(view_id)
        .and_then(|view| view.frame.as_ref().map(|frame| frame.seq))
}

fn frame(seq: u64, data_base64: String) -> Arc<FrameEvent> {
    Arc::new(FrameEvent {
        seq,
        width: 2,
        height: 2,
        viewport_width: 2,
        viewport_height: 2,
        data_base64,
    })
}

fn image(seq: u64) -> FrameImage {
    FrameImage {
        seq,
        width: 1,
        height: 1,
        viewport_width: 1,
        viewport_height: 1,
        image: Arc::new(image::DynamicImage::new_rgb8(1, 1)),
    }
}

fn tiny_jpeg() -> String {
    let source = image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]));
    let mut buffer = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new(&mut buffer)
        .encode_image(&source)
        .expect("encode jpeg");
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        peri_time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not reached before timeout");
}

/// 解码闸门：卡住指定帧的解码，直到测试放行（有超时兜底，避免测试挂死）。
#[derive(Default)]
struct Gate {
    open: parking_lot::Mutex<bool>,
    cv: parking_lot::Condvar,
}

impl Gate {
    fn wait(&self, timeout: Duration) {
        let mut open = self.open.lock();
        while !*open {
            if self.cv.wait_for(&mut open, timeout).timed_out() {
                return;
            }
        }
    }

    fn open(&self) {
        *self.open.lock() = true;
        self.cv.notify_all();
    }
}

#[serial_test::serial]
#[tokio::test]
async fn latest_frame_wins_and_stale_frames_are_dropped() {
    seed_view("v1");
    let gate = Arc::new(Gate::default());
    let decoded = Arc::new(Mutex::new(Vec::new()));
    let decode = {
        let gate = Arc::clone(&gate);
        let decoded = Arc::clone(&decoded);
        Arc::new(move |frame: &FrameEvent| {
            decoded.lock().push(frame.seq);
            if frame.seq == 1 {
                gate.wait(Duration::from_secs(5));
            }
            Some(image(frame.seq))
        })
    };
    let mut decoder = FrameDecoder::start("v1", CancellationToken::new(), decode);

    decoder.offer(frame(1, String::new()));
    wait_until(|| decoded.lock().contains(&1)).await;
    // 解码 1 在途时又到两帧：槽位只留最新一帧。
    decoder.offer(frame(2, String::new()));
    decoder.offer(frame(3, String::new()));
    gate.open();

    wait_until(|| applied_seq("v1") == Some(3)).await;
    assert_eq!(
        *decoded.lock(),
        vec![1, 3],
        "中间帧 2 必须被合并丢弃，只解最新帧"
    );
    decoder.finish(Duration::from_secs(1)).await;
}

#[serial_test::serial]
#[tokio::test]
async fn finish_decodes_the_last_frame_and_ignores_later_frames() {
    seed_view("v1");
    let mut decoder = FrameDecoder::start("v1", CancellationToken::new(), jpeg_decoder());
    decoder.offer(frame(7, tiny_jpeg()));
    // 收尾先解完在途帧：关闭前最后一帧仍会落地（只等一次解码）。
    decoder.finish(Duration::from_secs(2)).await;

    let view = APPS_STATE.state().read().view("v1").cloned().expect("view");
    assert_eq!(view.frame.as_ref().map(|frame| frame.seq), Some(7));
    assert_eq!(view.phase, ViewPhase::Ready, "首帧把 Starting 提升为 Ready");

    // 已结束：后续帧不再改变状态（幂等 finish）。
    decoder.offer(frame(8, tiny_jpeg()));
    decoder.finish(Duration::from_secs(1)).await;
    peri_time::sleep(Duration::from_millis(50)).await;
    assert_eq!(applied_seq("v1"), Some(7));
}

#[serial_test::serial]
#[tokio::test]
async fn finish_aborts_a_blocked_decode_without_hanging() {
    seed_view("v1");
    let gate = Arc::new(Gate::default());
    let decode = {
        let gate = Arc::clone(&gate);
        Arc::new(move |frame: &FrameEvent| {
            gate.wait(Duration::from_secs(5));
            Some(image(frame.seq))
        })
    };
    let mut decoder = FrameDecoder::start("v1", CancellationToken::new(), decode);
    decoder.offer(frame(1, String::new()));
    peri_time::sleep(Duration::from_millis(20)).await;

    let started = Instant::now();
    decoder.finish(Duration::from_millis(50)).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "收尾必须按预算返回（超时 abort 卡住的解码任务）"
    );
    gate.open();
}
