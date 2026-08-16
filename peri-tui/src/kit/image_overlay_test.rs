//! Tests for kit::image_overlay（image-p0-p1-spec §7.6 验收断言）。
//!
//! 覆盖：触发仲裁（hover > cursor > focus、遮挡）、cursor/focus 路径提取、
//! 几何、分级降级（Managed → Loading → Ready / Manual → Degraded / 校验失败
//! → Error）、陈旧解码结果丢弃、隐藏清理（Idle）、渲染（TestBackend：Ready
//! 含 `\x1b_G` transmit；Degraded/Idle 无 escape）。
//!
//! 全局 atom（IMAGE_PREVIEW_STATE / IMAGE_HOVER / INPUT_SNAPSHOT /
//! FOCUSED_ENTRY / VIEW_MODELS / POPUP_KIND / ACTIVE_PANEL）在测试间污染，
//! 统一 `reset_atoms()` + `serial_test::serial`（仿 mouse_router_test 模式）。

#[cfg(test)]
use super::*;
use crate::kit::atoms::{
    ACTIVE_PANEL, FOCUSED_ENTRY, IMAGE_HOVER, IMAGE_PREVIEW_STATE, INPUT_SNAPSHOT, POPUP_KIND,
    PopupKind, ViewModelsSnapshot,
};
use crate::kit::message_area::ImageHoverState;
use crate::kit::terminal_caps::GraphicsProtocol;
use crate::kit::tui_render_unit::TuiRenderUnit;
use image::{DynamicImage, ImageBuffer, Rgba};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serial_test::serial;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// 构造 w×h 纯色 RGBA 图片。
fn solid_image(w: u32, h: u32) -> DynamicImage {
    ImageBuffer::<Rgba<u8>, _>::from_pixel(w, h, Rgba([255, 0, 0, 255])).into()
}

/// 受管理根（tempdir 或任意目录）下写一张可解码 PNG，返回文件路径。
fn managed_fixture(dir: &Path, name: &str) -> PathBuf {
    let file = dir.join(name);
    solid_image(1, 1).save(&file).expect("fixture PNG 写入失败");
    file
}

/// 重置预览相关全局 atom，防测试间污染。
fn reset_atoms() {
    *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Idle;
    *IMAGE_HOVER.state().write() = None;
    *INPUT_SNAPSHOT.state().write() = InputSnapshot::default();
    *FOCUSED_ENTRY.state().write() = None;
    *POPUP_KIND.state().write() = None;
    *ACTIVE_PANEL.state().write() = None;
    *crate::kit::atoms::VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
}

/// 轮询 IMAGE_PREVIEW_STATE 直至谓词成立；超时返回 false。
///
/// 正常 <500ms；窗口放宽至 15s（1500×10ms）：解码跑独立 std::thread，
/// workspace 并发测试/CI 高负载下线程调度可能延迟数秒（本机压测观测到
/// 5s 窗口偶发超时）。
fn wait_state<F: Fn(&ImagePreviewState) -> bool>(pred: F) -> bool {
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
fn wait_for_resize(async_img: &mut AsyncImage) -> bool {
    for _ in 0..200 {
        if async_img.poll_completed() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// 统计 buffer 中所有 cell symbol 里的 kitty transmit 序列数（同 image_preview_test）。
fn count_transmit(buf: &Buffer) -> usize {
    buf.content()
        .iter()
        .filter(|c| c.symbol().contains("\x1b_G"))
        .count()
}

// ── cursor 触发源（§7.6 触发仲裁组）──────────────────────────────────

#[test]
fn cursor_image_path_finds_image_line() {
    let snap = InputSnapshot {
        text: "hello\n@image /tmp/a.png\nworld".to_string(),
        cursor_char: 20, // 第二行内
    };
    assert_eq!(cursor_image_path(&snap), Some("/tmp/a.png".to_string()));
}

#[test]
fn cursor_image_path_locates_line_by_char_index() {
    // 光标在"世界"行（第 2 行）→ 该行非 @image → None；光标在第 3 行 → 命中。
    let text = "@image /a.png\n世界\n@image /b.png".to_string();
    let line0_len = "@image /a.png".chars().count(); // 13
    let line1_len = "世界".chars().count(); // 2
    let c2 = line0_len + 1 + line1_len + 1; // 第 3 行起始 char 索引
    let snap = InputSnapshot {
        text,
        cursor_char: c2 + 1,
    };
    assert_eq!(cursor_image_path(&snap), Some("/b.png".to_string()));
    // 光标在第 2 行（非 @image）→ None。
    let snap2 = InputSnapshot {
        text: "@image /a.png\n世界\n@image /b.png".to_string(),
        cursor_char: line0_len + 1 + 1,
    };
    assert_eq!(cursor_image_path(&snap2), None);
}

#[test]
fn cursor_image_path_non_image_lines_return_none() {
    // 普通行 / 空文本 / @image 无路径 / @imagefoo → None。
    let cases = [
        InputSnapshot {
            text: "plain".into(),
            cursor_char: 0,
        },
        InputSnapshot::default(),
        InputSnapshot {
            text: "@image ".into(),
            cursor_char: 5,
        },
        InputSnapshot {
            text: "@imagefoo".into(),
            cursor_char: 3,
        },
    ];
    for c in cases {
        assert_eq!(cursor_image_path(&c), None, "text={:?}", c.text);
    }
}

#[test]
fn cursor_image_path_trims_whitespace() {
    let snap = InputSnapshot {
        text: "  @image   /tmp/a.png  ".to_string(),
        cursor_char: 5,
    };
    assert_eq!(cursor_image_path(&snap), Some("/tmp/a.png".to_string()));
}

// ── focus 触发源 ────────────────────────────────────────────────────────

/// 写 VIEW_MODELS 单 slot user bubble + FOCUSED_ENTRY 后断言 focus 路径。
fn view_models_with(text: &str) {
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(
        text.to_string(),
    ));
    *crate::kit::atoms::VIEW_MODELS.state().write() = ViewModelsSnapshot {
        items: im::Vector::from_iter([vm]),
        generation: 1,
    };
}

#[test]
fn focus_image_path_uses_first_image_line() {
    reset_atoms();
    view_models_with("text\n@image /tmp/b.png\n@image /tmp/c.png");
    let focused = FocusedEntry { slot: 0, key: None };
    assert_eq!(
        focus_image_path(&focused),
        Some("/tmp/b.png".to_string()),
        "取 slot 第一个 @image 行"
    );
}

#[test]
fn focus_image_path_slot_guards() {
    reset_atoms();
    view_models_with("@image /tmp/b.png");
    // 越界 slot → None。
    assert_eq!(focus_image_path(&FocusedEntry { slot: 9, key: None }), None);
    // 非 user bubble slot（再造一个 divider）→ None。
    let vm = TuiRenderUnit::TuiDivider(crate::kit::tui_render_unit::TuiDivider {
        label: None,
        content_hash: 1,
    });
    *crate::kit::atoms::VIEW_MODELS.state().write() = ViewModelsSnapshot {
        items: im::Vector::from_iter([vm]),
        generation: 1,
    };
    assert_eq!(focus_image_path(&FocusedEntry { slot: 0, key: None }), None);
}

// ── 三态仲裁 ────────────────────────────────────────────────────────────

#[test]
#[serial]
fn resolve_preview_target_priority_hover_cursor_focus() {
    reset_atoms();
    // 仅 cursor 命中 → cursor 路径。
    *INPUT_SNAPSHOT.state().write() = InputSnapshot {
        text: "@image /cur.png".to_string(),
        cursor_char: 0,
    };
    assert_eq!(resolve_preview_target(), Some("/cur.png".to_string()));

    // hover 与 cursor 同时命中 → hover 优先。
    *IMAGE_HOVER.state().write() = Some(ImageHoverState {
        row: 5,
        slot_index: 0,
        logical_idx: 1,
        vm_hash: 7,
        path: "/hov.png".to_string(),
        size_text: "45 B".to_string(),
    });
    assert_eq!(resolve_preview_target(), Some("/hov.png".to_string()));

    // hover 清空、cursor 命中、focus 命中 → cursor 优先于 focus。
    *IMAGE_HOVER.state().write() = None;
    view_models_with("@image /focus.png");
    *FOCUSED_ENTRY.state().write() = Some(FocusedEntry { slot: 0, key: None });
    assert_eq!(resolve_preview_target(), Some("/cur.png".to_string()));

    // cursor 清空 → focus。
    *INPUT_SNAPSHOT.state().write() = InputSnapshot::default();
    assert_eq!(resolve_preview_target(), Some("/focus.png".to_string()));

    // 全部清空 → None（Idle）。
    *FOCUSED_ENTRY.state().write() = None;
    assert_eq!(resolve_preview_target(), None);
}

#[test]
#[serial]
fn resolve_preview_target_occluded_returns_none() {
    reset_atoms();
    *INPUT_SNAPSHOT.state().write() = InputSnapshot {
        text: "@image /cur.png".to_string(),
        cursor_char: 0,
    };
    assert_eq!(resolve_preview_target(), Some("/cur.png".to_string()));
    *POPUP_KIND.state().write() = Some(PopupKind::Confirm);
    assert_eq!(resolve_preview_target(), None, "弹窗遮挡 → 隐藏（§7.5）");
}

// ── 几何（§7.4）─────────────────────────────────────────────────────────

#[test]
fn preview_geometry_centered_60x40() {
    let r = preview_geometry(100, 50).expect("正常终端应有几何");
    assert_eq!((r.width, r.height), (60, 20), "60% 宽 × 40% 高");
    assert_eq!((r.x, r.y), (20, 15), "居中");
}

#[test]
fn preview_geometry_saturates_small_terminals() {
    assert!(preview_geometry(3, 3).is_none(), "w<2 → 不渲染");
    assert!(preview_geometry(1, 100).is_none(), "h<2 → 不渲染");
    // 大终端不溢出（u16 乘法走 u32）。
    let r = preview_geometry(u16::MAX, u16::MAX).expect("大终端有几何");
    assert!(r.width > 0 && r.height > 0);
}

// ── 请求流程 / 分级降级（§7.6 状态机组）───────────────────────────────

#[test]
#[serial]
fn request_preview_managed_reaches_ready() {
    reset_atoms();
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().unwrap();
    let file = managed_fixture(dir.path(), "a.png");
    request_preview_with(Some(file.to_str().unwrap()), Some(dir.path()), decode_image);
    assert!(
        wait_state(|s| matches!(s, ImagePreviewState::Ready { .. })),
        "Managed fixture PNG 应解码到 Ready"
    );
    let state = IMAGE_PREVIEW_STATE.state().read().clone();
    match state {
        ImagePreviewState::Ready { path, meta, .. } => {
            assert_eq!(meta.width, 1, "1×1 fixture");
            assert_eq!(meta.height, 1);
            assert_eq!(meta.mime, "image/png");
            // macOS /var → /private/var：与 canonicalize 后的根比较。
            let canonical_root = dir
                .path()
                .canonicalize()
                .unwrap_or_else(|_| dir.path().to_path_buf());
            assert!(path.starts_with(&canonical_root), "canonical 后路径");
        }
        other => panic!("expected Ready, got {other:?}"),
    }
}

#[test]
#[serial]
fn request_preview_manual_degrades_without_decode() {
    reset_atoms();
    crate::i18n::init(Some("en"));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("managed");
    std::fs::create_dir(&root).unwrap();
    // 手工路径：受管理根之外（同 tempdir 但不在 managed 子目录内）。
    let file = managed_fixture(dir.path(), "manual.png");
    let called = std::sync::Arc::new(AtomicBool::new(false));
    let called2 = std::sync::Arc::clone(&called);
    request_preview_with(Some(file.to_str().unwrap()), Some(&root), move |_| {
        called2.store(true, Ordering::SeqCst);
        Err("unreachable")
    });
    assert!(
        wait_state(|s| matches!(s, ImagePreviewState::Degraded { .. })),
        "Manual 路径应 Degraded（不触发解码）"
    );
    assert!(!called.load(Ordering::SeqCst), "手工路径不得进入解码");
    let state = IMAGE_PREVIEW_STATE.state().read().clone();
    match state {
        ImagePreviewState::Degraded { reason, .. } => {
            assert_eq!(reason, crate::i18n::tr("image-preview-degraded"));
        }
        other => panic!("expected Degraded, got {other:?}"),
    }
}

#[test]
#[serial]
fn request_preview_validation_failure_errors() {
    reset_atoms();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("managed");
    std::fs::create_dir(&root).unwrap();
    // .png 扩展名但内容为文本 → T5 MimeMismatch → Error。
    let bad = root.join("fake.png");
    std::fs::write(&bad, "not an image").unwrap();
    request_preview_with(Some(bad.to_str().unwrap()), Some(&root), |_| {
        Err("unreachable")
    });
    assert!(
        wait_state(|s| matches!(s, ImagePreviewState::Error { .. })),
        "校验失败应 Error（安全降级）"
    );
}

#[test]
#[serial]
fn request_preview_invalid_path_errors() {
    reset_atoms();
    request_preview_with(Some("/nonexistent/nope.png"), None, |_| Err("unreachable"));
    assert!(
        wait_state(|s| matches!(s, ImagePreviewState::Error { .. })),
        "无法 canonicalize → Error"
    );
}

#[test]
#[serial]
fn request_preview_none_clears_to_idle() {
    reset_atoms();
    // 先制造一个非 Idle 状态（直接写 atom，避免依赖解码时序）。
    *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Error {
        path: "/tmp/a.png".into(),
        reason: "test".into(),
    };
    request_preview(None);
    assert!(
        matches!(
            IMAGE_PREVIEW_STATE.state().read().clone(),
            ImagePreviewState::Idle
        ),
        "触发清空 → Idle（隐藏清理）"
    );
}

/// 陈旧结果丢弃（§7.5）：请求 A（解码阻塞）后请求 B（立即完成）；
/// B 写入 Ready 后放行 A——A 的完成结果必须被丢弃，状态保持 B。
#[test]
#[serial]
fn stale_decode_result_is_dropped() {
    reset_atoms();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("managed");
    std::fs::create_dir(&root).unwrap();
    let file_a = managed_fixture(&root, "a.png");
    let file_b = managed_fixture(&root, "b.png");

    let (tx_release, rx_release) = mpsc::channel::<()>();
    request_preview_with(Some(file_a.to_str().unwrap()), Some(&root), move |_| {
        let _ = rx_release.recv(); // 阻塞直至主线程放行
        Ok(solid_image(2, 2))
    });
    request_preview_with(Some(file_b.to_str().unwrap()), Some(&root), |_| {
        Ok(solid_image(4, 4))
    });
    assert!(
        wait_state(
            |s| matches!(s, ImagePreviewState::Ready { path, .. } if path.ends_with("b.png"))
        ),
        "B 应先完成并写入 Ready"
    );
    tx_release.send(()).unwrap(); // 放行 A 的陈旧解码
    // 等待 A 线程完成（其写回前请求 id 已不匹配 → 丢弃）。
    std::thread::sleep(Duration::from_millis(150));
    let state = IMAGE_PREVIEW_STATE.state().read().clone();
    match state {
        ImagePreviewState::Ready { path, .. } => {
            assert!(path.ends_with("b.png"), "陈旧结果被丢弃，保持 B")
        }
        other => panic!("expected Ready(b), got {other:?}"),
    }
}

// ── 渲染（TestBackend，§7.6 渲染测试组）───────────────────────────────

/// 仅覆盖 graphics 字段的能力集合（其余默认全能力）。
fn caps(graphics: GraphicsProtocol) -> crate::kit::terminal_caps::TerminalCaps {
    crate::kit::terminal_caps::TerminalCaps {
        graphics,
        ..Default::default()
    }
}

/// 构造 Ready 状态（路径 + meta + 图片）。
fn ready_state(path: PathBuf) -> ImagePreviewState {
    ImagePreviewState::Ready {
        path,
        meta: crate::kit::image_safety::ImageMeta {
            width: 1,
            height: 1,
            size_bytes: 68,
            mime: "image/png",
        },
        img: Arc::new(solid_image(64, 64)),
    }
}

#[test]
fn draw_preview_overlay_ready_transmits() {
    crate::i18n::init(Some("en"));
    let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
    let rect = Rect::new(10, 4, 24, 12);
    let state = ready_state(PathBuf::from("/tmp/a.png"));
    let mut async_img = AsyncImage::new(
        &picker_for(&caps(GraphicsProtocol::Kitty)),
        solid_image(64, 64),
    );
    let caps = caps(GraphicsProtocol::Kitty);
    // 首帧：后台 resize/encode 完成前无编码帧（同 image_preview_test 时序）；
    // 先发出首帧请求并等后台完成，再断言 overlay 输出 transmit。
    let mut seed = Buffer::empty(Rect::new(0, 0, 40, 20));
    async_img.render(Rect::new(11, 6, 22, 9), &mut seed);
    assert!(
        wait_for_resize(&mut async_img),
        "后台 resize 应在超时内完成"
    );
    term.draw(|f| {
        draw_preview_overlay(
            f.buffer_mut(),
            rect,
            &state,
            true,
            Some(&mut async_img),
            &caps,
        )
    })
    .unwrap();
    let buf = term.backend().buffer();
    // 首帧 transmit 恰好 1 处（y==0 首行 cell——overlay 内首行即 rect.y）。
    assert_eq!(count_transmit(buf), 1, "Ready + Kitty 应输出像素 transmit");
    // 边框可见（Rounded 圆角：左上角 ╭）。
    assert!(
        buf.content().iter().any(|c| c.symbol() == "╭"),
        "边框左上角存在"
    );
}

#[test]
fn draw_preview_overlay_degraded_no_escape() {
    crate::i18n::init(Some("en"));
    let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
    let rect = Rect::new(10, 4, 24, 12);
    let state = ImagePreviewState::Degraded {
        path: "/tmp/a.png".into(),
        reason: crate::i18n::tr("image-preview-degraded"),
    };
    let caps = caps(GraphicsProtocol::Kitty);
    term.draw(|f| draw_preview_overlay(f.buffer_mut(), rect, &state, true, None, &caps))
        .unwrap();
    let buf = term.backend().buffer();
    assert_eq!(count_transmit(buf), 0, "Degraded 无像素 escape");
    // meta 行提示文本渲染在边框内首行（按列遍历该行 cell 收集符号）。
    let meta = crate::i18n::tr("image-preview-degraded");
    let row_text: String = (rect.x..rect.x + rect.width)
        .map(|x| buf[(x, rect.y + 1)].symbol().to_string())
        .collect();
    assert!(
        row_text.contains(&meta[..meta.len().min(8)]),
        "提示文本出现"
    );
}

#[test]
fn draw_preview_overlay_idle_empty() {
    crate::i18n::init(Some("en"));
    let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
    let rect = Rect::new(10, 4, 24, 12);
    let caps = caps(GraphicsProtocol::Kitty);
    term.draw(|f| {
        draw_preview_overlay(
            f.buffer_mut(),
            rect,
            &ImagePreviewState::Idle,
            true,
            None,
            &caps,
        )
    })
    .unwrap();
    assert_eq!(count_transmit(term.backend().buffer()), 0, "Idle 无输出");
    assert!(
        !term
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|c| c.symbol() == "┌"),
        "Idle 不画边框"
    );
}

#[test]
fn draw_preview_overlay_pixel_disabled_meta_only() {
    crate::i18n::init(Some("en"));
    let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
    let rect = Rect::new(10, 4, 24, 12);
    let state = ready_state(PathBuf::from("/tmp/a.png"));
    let mut async_img = AsyncImage::new(
        &picker_for(&caps(GraphicsProtocol::Kitty)),
        solid_image(64, 64),
    );
    let caps = caps(GraphicsProtocol::Kitty);
    term.draw(|f| {
        draw_preview_overlay(
            f.buffer_mut(),
            rect,
            &state,
            false, // 终端小于最小尺寸 → 仅 meta 行（§7.4 纯文本降级）
            Some(&mut async_img),
            &caps,
        )
    })
    .unwrap();
    assert_eq!(
        count_transmit(term.backend().buffer()),
        0,
        "像素禁用时不 transmit"
    );
}

#[test]
fn draw_preview_overlay_unsupported_protocol_degrades() {
    crate::i18n::init(Some("en"));
    let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
    let rect = Rect::new(10, 4, 24, 12);
    let state = ready_state(PathBuf::from("/tmp/a.png"));
    let caps = caps(GraphicsProtocol::ITerm2); // 检测但 disabled（§6.3）
    term.draw(|f| draw_preview_overlay(f.buffer_mut(), rect, &state, true, None, &caps))
        .unwrap();
    assert_eq!(
        count_transmit(term.backend().buffer()),
        0,
        "无协议/disabled → 文本降级"
    );
}

/// [方案 B] 无协议终端：像素区显示终端能力提示（而非静默空白），
/// 且不 transmit。同时覆盖 ITerm2 disabled（同分支，§6.3）。
#[test]
fn draw_preview_overlay_no_protocol_shows_hint() {
    crate::i18n::init(Some("en"));
    for protocol in [GraphicsProtocol::None, GraphicsProtocol::ITerm2] {
        let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
        let rect = Rect::new(10, 4, 24, 12);
        let state = ready_state(PathBuf::from("/tmp/a.png"));
        let caps = caps(protocol);
        term.draw(|f| draw_preview_overlay(f.buffer_mut(), rect, &state, true, None, &caps))
            .unwrap();
        assert_eq!(
            count_transmit(term.backend().buffer()),
            0,
            "{protocol:?} 不 transmit"
        );
        let content: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("terminal doesn'"),
            "{protocol:?} 能力提示应出现在像素区（宽截断后前缀），实际: {content}"
        );
    }
}

#[test]
fn preview_meta_text_ready_format() {
    crate::i18n::init(Some("en"));
    let text = preview_meta_text(&ready_state(PathBuf::from("/tmp/a.png")));
    assert!(
        text.starts_with("[Image: a.png · 1×1"),
        "meta 行含文件名 + WxH，实际: {text}"
    );
    assert!(text.contains("image/png"), "meta 行含 MIME，实际: {text}");
    // 路径泄漏约束（§6.2-5）：不暴露绝对路径。
    assert!(
        !text.contains("/tmp/"),
        "meta 行不得含绝对路径，实际: {text}"
    );
}

// ── 评审回归（review-p0 / review-p1）──────────────────────────────────

/// F1 回归：Idle（无预览，绝大多数时间）时组件不得返回全尺寸 `clear: true`
/// 的 Positioned——`Positioned::draw` 对 clear: true 无条件清空该区域，
/// 终端中央 60%×40% 下层消息会被每帧擦除（§7.5 隐藏应恢复原内容）。
/// 非 Idle 才返回 clear 区域；终端过小（无几何）恒不渲染。
#[test]
fn idle_does_not_clear_overlay_area() {
    let rect = preview_geometry(100, 40).expect("大终端有几何");
    assert_eq!(
        overlay_clear_rect(Some(rect), &ImagePreviewState::Idle),
        None,
        "Idle 必须走 0×0 clear:false 空覆盖（F1）"
    );
    let state = ready_state(PathBuf::from("/tmp/a.png"));
    assert_eq!(
        overlay_clear_rect(Some(rect), &state),
        Some(rect),
        "Ready 才返回全尺寸 clear 覆盖"
    );
    assert_eq!(
        overlay_clear_rect(None, &state),
        None,
        "终端过小无几何 → 恒空覆盖"
    );
}

/// F2 回归（TOCTOU 闭合）：decode 必须消费 T5 校验时读入的缓冲，不得按
/// 路径二次打开——validate 通过后、decode 执行前文件被替换成超大内容时，
/// decode 仍应解码原始缓冲（字节上限 ≤ MAX_IMAGE_BYTES 天然生效）。
#[test]
#[serial]
fn decode_uses_validated_buffer_not_reopened_path() {
    reset_atoms();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("managed");
    std::fs::create_dir(&root).unwrap();
    let file = managed_fixture(&root, "a.png");
    let original = std::fs::read(&file).unwrap();
    let (tx_started, rx_started) = mpsc::channel::<()>();
    let (tx_release, rx_release) = mpsc::channel::<()>();
    request_preview_with(Some(file.to_str().unwrap()), Some(&root), move |buf| {
        tx_started.send(()).unwrap();
        rx_release.recv().unwrap(); // 等主线程替换磁盘文件
        assert_eq!(
            buf, original,
            "decode 必须使用 validate 时读入的缓冲（F2 TOCTOU）"
        );
        Ok(solid_image(1, 1))
    });
    rx_started.recv().unwrap();
    // 校验已完成、decode 闭包已持有缓冲：把磁盘文件替换为 1MB 内容。
    std::fs::write(&file, vec![0u8; 1024 * 1024]).unwrap();
    tx_release.send(()).unwrap();
    assert!(
        wait_state(|s| matches!(s, ImagePreviewState::Ready { .. })),
        "decode 从缓冲解码应达 Ready（未按路径重开被替换文件）"
    );
}

/// F3 回归（§6.2-4）：overlay meta 行必须过 `sanitize_for_terminal`——文件名
/// 含 ESC 等控制字符（macOS 允许）时，escape 不得随 meta 行进入终端 buffer。
#[test]
fn preview_meta_line_sanitizes_control_chars() {
    crate::i18n::init(Some("en"));
    let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
    let rect = Rect::new(10, 4, 24, 12);
    let caps = caps(GraphicsProtocol::Kitty);
    let state = ImagePreviewState::Loading {
        path: "/tmp/a\x1b[31m.png".into(),
        grade: crate::kit::image_safety::PathGrade::Managed,
    };
    term.draw(|f| draw_preview_overlay(f.buffer_mut(), rect, &state, true, None, &caps))
        .unwrap();
    let buf = term.backend().buffer();
    let row_text: String = (rect.x..rect.x + rect.width)
        .map(|x| buf[(x, rect.y + 1)].symbol().to_string())
        .collect();
    assert!(
        !row_text.contains('\x1b'),
        "meta 行不得含 ESC 序列（F3），实际: {row_text:?}"
    );
}
