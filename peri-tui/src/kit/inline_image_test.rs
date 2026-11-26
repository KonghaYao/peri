//! 消息区图片扫描：独占行判定、VM 图片门控、请求预算。
//!
//! 缓存/编码/几何在 `kit::image_view` 内测试；这里只覆盖「这条消息有哪些图」。

use crate::kit::image_view::{ImageSource, UploadMedia, lookup};
use crate::kit::tui_render_unit::{TuiToolCard, TuiUserBubble};

use super::*;

fn user_bubble(
    text: &str,
    attachments: Vec<crate::kit::tui_render_unit::UserAttachment>,
) -> TuiRenderUnit {
    TuiRenderUnit::TuiUserBubble(TuiUserBubble::with_attachments(
        text.to_string(),
        attachments,
    ))
}

fn tool_card(images: Vec<crate::kit::image_view::UploadId>) -> TuiRenderUnit {
    TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "t1".into(),
        tool_name: "Read".into(),
        input_summary: String::new(),
        output_summary: String::new(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: None,
        diff: None,
        presentation: crate::kit::tui_render_unit::TuiToolPresentation::Generic,
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        user_modified: false,
        images,
        content_hash: 0,
        tool_calls_count: 0,
    })
}

#[test]
fn standalone_image_line_requires_exclusive_syntax() {
    assert_eq!(
        standalone_image_url("![a](a.png)").as_deref(),
        Some("a.png")
    );
    assert_eq!(
        standalone_image_url("  ![a](a.png)  ").as_deref(),
        Some("a.png")
    );
    assert_eq!(standalone_image_url("see ![alt](a.png)"), None);
    assert_eq!(standalone_image_url("![a](p) ![b](q)"), None);
    assert_eq!(standalone_image_url("plain text"), None);
    assert_eq!(standalone_image_url("![a]()"), None);
}

#[test]
fn vm_may_have_image_covers_attachments_and_tool_images() {
    crate::i18n::init(Some("en"));
    assert!(vm_may_have_image(&user_bubble("![a](a.png)", Vec::new())));
    let attachment = crate::kit::tui_render_unit::UserAttachment::for_bytes(48);
    assert!(
        vm_may_have_image(&user_bubble("hi", vec![attachment])),
        "上传附件没有图片语法，也必须触发缓存失效"
    );
    let id = crate::kit::image_view::UploadId {
        digest: 7,
        media: UploadMedia::Png,
    };
    assert!(vm_may_have_image(&tool_card(vec![id])));
    assert!(!vm_may_have_image(&tool_card(Vec::new())));
    assert!(!vm_may_have_image(&user_bubble("hi", Vec::new())));
}

#[test]
#[serial_test::serial]
fn request_vm_images_without_protocol_does_not_register() {
    crate::i18n::init(Some("en"));
    crate::kit::atoms::init_atoms();
    let saved = *crate::kit::atoms::TERMINAL_CAPS.state().read();
    crate::kit::atoms::TERMINAL_CAPS.set(crate::kit::terminal_caps::TerminalCaps::default());
    request_vm_images(&user_bubble("![a](/tmp/never.png)", Vec::new()), 40, 30);
    crate::kit::atoms::TERMINAL_CAPS.set(saved);
    assert!(
        lookup(
            &ImageSource::File("/tmp/never.png"),
            crate::kit::image_view::Budget::new(40, MAX_ROWS)
        )
        .is_none(),
        "无协议不得登记请求"
    );
}
