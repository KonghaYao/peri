//! 附件浮层预览测试：composer 待发送附件（粘贴流）与消息区已提交附件记录行
//! （有界保留层数据源）。共享夹具见 `image_overlay_test_support.rs`。

use super::test_support::*;
use super::*;
use crate::kit::atoms::{FOCUSED_ENTRY, IMAGE_PREVIEW_HOVER, IMAGE_PREVIEW_STATE, INPUT_SNAPSHOT};
use crate::kit::message_area::ImageHoverState;
use crate::kit::terminal_caps::GraphicsProtocol;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use serial_test::serial;
use std::sync::Arc;

// ── 粘贴流：待发送附件的浮动预览（回归：上传式附件曾无任何预览来源）──────

/// 待发送附件夹具：`AQID` 不是合法 PNG（只用于触发/身份用例）；真实 PNG
/// 由 `base64_std` + `solid_image` 生成。
fn pending_image(base64_data: &str) -> crate::kit::atoms::PendingAttachment {
    crate::kit::atoms::PendingAttachment::image("image/png", base64_data)
}

/// 真实 PNG 的标准 base64（供附件解码路径使用）。
fn solid_png_base64() -> String {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.png");
    solid_image(2, 3).save(&file).expect("fixture PNG 写入失败");
    let bytes = std::fs::read(&file).unwrap();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// composer 空文本 + 有附件 → 预览最后一张（刚粘贴的那张），且带载荷身份。
#[test]
#[serial]
fn pending_attachment_target_previews_last_when_composer_empty() {
    reset_atoms();
    let state = crate::kit::atoms::PENDING_ATTACHMENTS.state();
    {
        let mut pending = state.write();
        pending.clear();
        pending.push(pending_image("AQID"));
        pending.push(pending_image("BAUG"));
    }

    let snapshot = InputSnapshot::default();
    assert_eq!(
        pending_attachment_target(&snapshot),
        Some(PreviewTarget::PendingImage {
            index: 1,
            payload_id: crate::kit::atoms::payload_id_of("BAUG"),
        }),
        "预览最后一张附件"
    );

    // 仲裁：无 hover/cursor/focus 时附件源生效。
    *INPUT_SNAPSHOT.state().write() = snapshot.clone();
    assert!(matches!(
        resolve_preview_target(),
        Some(PreviewTarget::PendingImage { index: 1, .. })
    ));

    state.write().clear();
}

/// 一旦开始输入文本即隐藏（附件预览不遮挡正文）；无附件时无目标。
#[test]
#[serial]
fn pending_attachment_target_hidden_with_text_or_without_attachments() {
    reset_atoms();
    let state = crate::kit::atoms::PENDING_ATTACHMENTS.state();
    {
        let mut pending = state.write();
        pending.clear();
        pending.push(pending_image("AQID"));
    }
    let typed = InputSnapshot {
        text: "看一下这张图".to_string(),
        cursor_char: 6,
    };
    assert_eq!(pending_attachment_target(&typed), None, "有文本 → 不预览");

    state.write().clear();
    assert_eq!(
        pending_attachment_target(&InputSnapshot::default()),
        None,
        "无附件 → 无目标"
    );
}

/// 优先级：光标所在 `@image` 行优先于待发送附件（同类 composer 来源，
/// 显式路径指哪看哪）；附件又优先于焦点 entry。
#[test]
#[serial]
fn resolve_preview_target_priority_attachment_between_cursor_and_focus() {
    reset_atoms();
    let state = crate::kit::atoms::PENDING_ATTACHMENTS.state();
    {
        let mut pending = state.write();
        pending.clear();
        pending.push(pending_image("AQID"));
    }
    view_models_with("@image /focus.png");
    *FOCUSED_ENTRY.state().write() = Some(FocusedEntry { slot: 0, key: None });

    // cursor 命中 → 路径优先于附件。
    *INPUT_SNAPSHOT.state().write() = InputSnapshot {
        text: "@image /cur.png".to_string(),
        cursor_char: 0,
    };
    assert_eq!(
        resolve_preview_target(),
        Some(PreviewTarget::Path("/cur.png".to_string()))
    );

    // cursor 空文本（附件在场）→ 附件优先于 focus。
    *INPUT_SNAPSHOT.state().write() = InputSnapshot::default();
    assert!(matches!(
        resolve_preview_target(),
        Some(PreviewTarget::PendingImage { index: 0, .. })
    ));

    // 附件清空 → 回到 focus。
    state.write().clear();
    assert_eq!(
        resolve_preview_target(),
        Some(PreviewTarget::Path("/focus.png".to_string()))
    );
    *FOCUSED_ENTRY.state().write() = None;
}

/// 请求链路：附件载荷解码到 Ready（展示名 = `图片 N`，meta 来自解码结果）。
#[test]
#[serial]
fn request_pending_attachment_preview_reaches_ready() {
    reset_atoms();
    crate::i18n::init(Some("zh-CN"));
    let payload = solid_png_base64();
    let state = crate::kit::atoms::PENDING_ATTACHMENTS.state();
    {
        let mut pending = state.write();
        pending.clear();
        pending.push(pending_image(&payload));
    }

    request_preview(Some(PreviewTarget::PendingImage {
        index: 0,
        payload_id: crate::kit::atoms::payload_id_of(&payload),
    }));

    assert!(
        wait_state(|s| matches!(s, ImagePreviewState::Ready { .. })),
        "附件载荷应解码到 Ready（不依赖磁盘路径）"
    );
    match IMAGE_PREVIEW_STATE.state().read().clone() {
        ImagePreviewState::Ready { source, meta, .. } => {
            assert_eq!(source.display_name(), "图片 1", "展示名用附件序号");
            assert!(
                source.display_name().chars().all(|c| c != '/'),
                "meta 行不得暴露路径"
            );
            assert_eq!((meta.width, meta.height), (2, 3), "尺寸来自解码结果");
            assert_eq!(meta.mime, "image/png");
            assert!(meta.size_bytes > 0);
        }
        other => panic!("应为 Ready，实际 {other:?}"),
    }
    state.write().clear();
}

/// 陈旧请求（附件已被移除或替换）不得显示过期图片 → Idle。
#[test]
#[serial]
fn request_pending_attachment_preview_stale_payload_goes_idle() {
    reset_atoms();
    let state = crate::kit::atoms::PENDING_ATTACHMENTS.state();
    {
        let mut pending = state.write();
        pending.clear();
        pending.push(pending_image("AQID"));
    }

    request_preview(Some(PreviewTarget::PendingImage {
        index: 0,
        payload_id: crate::kit::atoms::payload_id_of("BAUG"),
    }));
    assert!(
        matches!(*IMAGE_PREVIEW_STATE.state().read(), ImagePreviewState::Idle),
        "载荷身份不匹配 → Idle（不显示过期图片）"
    );

    request_preview(Some(PreviewTarget::PendingImage {
        index: 3,
        payload_id: crate::kit::atoms::payload_id_of("AQID"),
    }));
    assert!(
        matches!(*IMAGE_PREVIEW_STATE.state().read(), ImagePreviewState::Idle),
        "越界索引 → Idle"
    );
    state.write().clear();
}

/// 非图片载荷（base64 不是图片）→ Error，不静默失败。
#[test]
#[serial]
fn request_pending_attachment_preview_bad_payload_errors() {
    reset_atoms();
    crate::i18n::init(Some("en"));
    let state = crate::kit::atoms::PENDING_ATTACHMENTS.state();
    {
        let mut pending = state.write();
        pending.clear();
        pending.push(pending_image("bm90LWEtcG5n"));
    }
    request_preview(Some(PreviewTarget::PendingImage {
        index: 0,
        payload_id: crate::kit::atoms::payload_id_of("bm90LWEtcG5n"),
    }));
    assert!(
        wait_state(|s| matches!(s, ImagePreviewState::Error { .. })),
        "非图片载荷应报 Error"
    );
    state.write().clear();
}

/// 渲染路径：附件来源的 meta 行显示展示名（`图片 1`）而非路径。
#[test]
#[serial]
fn preview_meta_line_shows_attachment_name() {
    crate::i18n::init(Some("zh-CN"));
    let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
    let rect = Rect::new(10, 4, 24, 12);
    let caps = caps(GraphicsProtocol::Kitty);
    let state = ImagePreviewState::Ready {
        source: crate::kit::atoms::PreviewSource::Pending {
            name: "图片 1".to_string(),
            payload_id: 7,
        },
        meta: crate::kit::image_safety::ImageMeta {
            width: 2,
            height: 3,
            size_bytes: 68,
            mime: "image/png",
        },
        img: Arc::new(solid_image(2, 3)),
    };
    term.draw(|f| draw_preview_overlay(f.buffer_mut(), rect, &state, true, None, &caps))
        .unwrap();
    let buf = term.backend().buffer();
    let row_text: String = (rect.x..rect.x + rect.width)
        .map(|x| buf[(x, rect.y + 1)].symbol().to_string())
        .collect();
    // 逐格取样会为宽字符补位（CJK 后跟空格），比较前先去空白；期望文本按当前
    // 语言构造，避免与并行测试的 i18n 初始化相互干扰。
    let expected = i18n::tr_args(
        "user-image-meta",
        &[
            (
                "name".to_string(),
                FluentValue::from(crate::kit::tui_render_unit::user_attachment_name(1)),
            ),
            ("size".to_string(), FluentValue::from("68 B")),
        ],
    );
    let expected_display = expected
        .trim_start_matches('[')
        .split(" · ")
        .next()
        .unwrap_or_default()
        .replace(' ', "");
    assert!(
        row_text.replace(' ', "").contains(&expected_display),
        "meta 行应显示附件展示名（{expected_display}），实际: {row_text:?}"
    );
    assert!(!row_text.contains('/'), "meta 行不得出现路径: {row_text:?}");
}

// ── 消息区附件记录行：hover → 保留载荷 → Ready（有界保留层数据源）──────

/// hover 命中附件记录行（`ImageLineIdentity::Attachment`）→ 预览目标按
/// `payload_id` 解析为已提交附件（而非路径）。
#[test]
#[serial]
fn resolve_preview_target_maps_attachment_hover_to_submitted_image() {
    reset_atoms();
    *IMAGE_PREVIEW_HOVER.state().write() = Some(ImageHoverState {
        row: 5,
        slot_index: 0,
        logical_idx: 1,
        vm_hash: 7,
        identity: crate::kit::message_area::ImageLineIdentity::Attachment {
            name: crate::kit::tui_render_unit::user_attachment_name(2),
            payload_id: 99,
        },
        size_text: "45 B".to_string(),
    });

    assert_eq!(
        resolve_preview_target(),
        Some(PreviewTarget::SubmittedImage {
            name: crate::kit::tui_render_unit::user_attachment_name(2),
            payload_id: 99,
        }),
        "附件记录行 hover → 已提交附件目标（身份用 payload_id）"
    );
}

/// 请求链路：保留层有载荷 → Ready（展示名与身份来自记录行）。
#[test]
#[serial]
fn request_submitted_attachment_preview_reaches_ready() {
    reset_atoms();
    crate::i18n::init(Some("zh-CN"));
    let payload = solid_png_base64();
    let payload_id = crate::kit::atoms::payload_id_of(&payload);
    crate::kit::atoms::RETAINED_ATTACHMENTS
        .state()
        .write()
        .retain("image/png", &payload, payload_id);

    request_preview(Some(PreviewTarget::SubmittedImage {
        name: "图片 2".to_string(),
        payload_id,
    }));

    assert!(
        wait_state(|s| matches!(s, ImagePreviewState::Ready { .. })),
        "保留载荷应解码到 Ready"
    );
    match IMAGE_PREVIEW_STATE.state().read().clone() {
        ImagePreviewState::Ready { source, meta, .. } => {
            assert_eq!(source.display_name(), "图片 2");
            assert!(
                matches!(
                    &source,
                    crate::kit::atoms::PreviewSource::Submitted { payload_id: id, .. }
                        if *id == payload_id
                ),
                "来源身份应为已提交附件且 payload_id 一致，实际 {source:?}"
            );
            assert_eq!((meta.width, meta.height), (2, 3));
        }
        other => panic!("应为 Ready，实际 {other:?}"),
    }
    crate::kit::atoms::RETAINED_ATTACHMENTS
        .state()
        .write()
        .clear();
}

/// 被淘汰/未保留的身份 → Idle（**绝不显示别的图片**）。
#[test]
#[serial]
fn request_submitted_attachment_preview_evicted_goes_idle() {
    reset_atoms();
    let atom = crate::kit::atoms::RETAINED_ATTACHMENTS.state();
    {
        let mut retained = atom.write();
        retained.clear();
    }

    request_preview(Some(PreviewTarget::SubmittedImage {
        name: "图片 1".to_string(),
        payload_id: 12345,
    }));
    assert!(
        matches!(*IMAGE_PREVIEW_STATE.state().read(), ImagePreviewState::Idle),
        "未保留载荷 → Idle（不预览、不报错）"
    );

    // 曾经保留过、随后被会话边界清空 → 同样退化为 Idle。
    let payload = solid_png_base64();
    let payload_id = crate::kit::atoms::payload_id_of(&payload);
    atom.write().retain("image/png", &payload, payload_id);
    *IMAGE_PREVIEW_STATE.state().write() = ImagePreviewState::Ready {
        source: crate::kit::atoms::PreviewSource::Submitted {
            name: "图片 1".to_string(),
            payload_id,
        },
        meta: crate::kit::image_safety::ImageMeta {
            width: 1,
            height: 1,
            size_bytes: 68,
            mime: "image/png",
        },
        img: Arc::new(solid_image(1, 1)),
    };
    atom.write().clear();

    request_preview(Some(PreviewTarget::SubmittedImage {
        name: "图片 1".to_string(),
        payload_id,
    }));
    assert!(
        matches!(*IMAGE_PREVIEW_STATE.state().read(), ImagePreviewState::Idle),
        "清空后同一身份请求 → Idle（不回退到旧画面）"
    );
}
