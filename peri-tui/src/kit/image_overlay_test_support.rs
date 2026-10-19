//! image_overlay 共享测试夹具（`image_overlay_test.rs` 与
//! `image_overlay_attachment_test.rs` 共用，避免两份漂移）。

#![allow(dead_code)] // 各测试模块只用其中一部分

use super::*;
use crate::kit::atoms::{
    ACTIVE_PANEL, FOCUSED_ENTRY, IMAGE_HOVER, IMAGE_PREVIEW_HOVER, IMAGE_PREVIEW_STATE,
    INPUT_SNAPSHOT, POPUP_KIND, ViewModelsSnapshot,
};
use crate::kit::terminal_caps::GraphicsProtocol;
use crate::kit::tui_render_unit::TuiRenderUnit;
use image::{DynamicImage, ImageBuffer, Rgba};
use ratatui::buffer::Buffer;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 构造 w×h 纯色 RGBA 图片。
pub(super) fn solid_image(w: u32, h: u32) -> DynamicImage {
    ImageBuffer::<Rgba<u8>, _>::from_pixel(w, h, Rgba([255, 0, 0, 255])).into()
}

/// 受管理根（tempdir 或任意目录）下写一张可解码 PNG，返回文件路径。
pub(super) fn managed_fixture(dir: &Path, name: &str) -> PathBuf {
    let file = dir.join(name);
    solid_image(1, 1).save(&file).expect("fixture PNG 写入失败");
    file
}

/// 重置预览相关全局 atom，防测试间污染。
pub(super) fn reset_atoms() {
    *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Idle;
    *IMAGE_HOVER.state().write() = None;
    *IMAGE_PREVIEW_HOVER.state().write() = None;
    *INPUT_SNAPSHOT.state().write() = InputSnapshot::default();
    *FOCUSED_ENTRY.state().write() = None;
    *POPUP_KIND.state().write() = None;
    *ACTIVE_PANEL.state().write() = None;
    *crate::kit::atoms::VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    // 第四触发源与附件保留层是同级全局状态：不清理会被其它 serial 用例
    // （如 submit 的「控制请求保留附件」）遗留，导致仲裁断言串味。
    crate::kit::atoms::PENDING_ATTACHMENTS
        .state()
        .write()
        .clear();
    crate::kit::atoms::RETAINED_ATTACHMENTS
        .state()
        .write()
        .clear();
}

/// 轮询 IMAGE_PREVIEW_STATE 直至谓词成立；超时返回 false。
///
/// 正常 <500ms；窗口放宽至 15s（1500×10ms）：解码跑独立 std::thread，
/// workspace 并发测试/CI 高负载下线程调度可能延迟数秒（本机压测观测到
/// 5s 窗口偶发超时）。
pub(super) fn wait_state<F: Fn(&ImagePreviewState) -> bool>(pred: F) -> bool {
    for _ in 0..1500 {
        let state = IMAGE_PREVIEW_STATE.state().read().clone();
        if pred(&state) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// 轮询后台 resize 线程直至结果应用；超时返回 false（正常环境 <100ms）。
pub(super) fn wait_for_resize(async_img: &mut AsyncImage) -> bool {
    for _ in 0..200 {
        if async_img.poll_completed() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// 统计 buffer 中所有 cell symbol 里的 kitty transmit 序列数（同 image_preview_test）。
pub(super) fn count_transmit(buf: &Buffer) -> usize {
    buf.content()
        .iter()
        .filter(|c| c.symbol().contains("\x1b_G"))
        .count()
}

/// 仅覆盖 graphics 字段的能力集合（其余默认全能力）。
pub(super) fn caps(graphics: GraphicsProtocol) -> crate::kit::terminal_caps::TerminalCaps {
    crate::kit::terminal_caps::TerminalCaps {
        graphics,
        ..Default::default()
    }
}

/// 写 VIEW_MODELS 单 slot user bubble + FOCUSED_ENTRY 后断言 focus 路径。
pub(super) fn view_models_with(text: &str) {
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(
        text.to_string(),
    ));
    *crate::kit::atoms::VIEW_MODELS.state().write() = ViewModelsSnapshot {
        items: im::Vector::from_iter([vm]),
        generation: 1,
    };
}
