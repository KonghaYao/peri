//! 附件展示与载荷保留的路径一致性测试（提交 / 投递两条写入点）。
//!
//! 从 `steer_state_test.rs` 拆出（STD-SIZE-001）；`super` 为 `kit::steer_state`。

use super::*;
use serial_test::serial;

/// 提交路径的载荷保留：按 `payload_id` 存入有界保留层，与展示记录身份一致。
#[test]
#[serial]
fn retain_submitted_attachments_keeps_payloads_by_identity() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::RETAINED_ATTACHMENTS
        .state()
        .write()
        .clear();

    let attachments = vec![
        PendingAttachment::image("image/png", "AQID"),
        PendingAttachment::image("image/png", "BAUG"),
    ];
    let displays = attachment_displays(&attachments);
    retain_submitted_attachments(&attachments);

    let atom = crate::kit::atoms::RETAINED_ATTACHMENTS.state();
    let retained = atom.read();
    for (attachment, display) in attachments.iter().zip(displays.iter()) {
        assert_eq!(
            attachment.payload_id, display.payload_id,
            "展示记录身份必须与保留层一致（否则预览取不到载荷）"
        );
        let entry = retained
            .get(display.payload_id)
            .expect("已提交附件应可被记录行按身份取回");
        assert_eq!(entry.base64_data, attachment.base64_data);
        assert_eq!(entry.media_type, "image/png");
    }
    drop(retained);
    atom.write().clear();
}

/// 投递路径（用户输入队列）同样建立保留：身份与展示记录同源，避免两条路径漂移。
#[test]
#[serial]
fn retain_submitted_from_content_matches_display_identity() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::RETAINED_ATTACHMENTS
        .state()
        .write()
        .clear();

    let attachments = vec![PendingAttachment::image("image/png", "AQID")];
    let content = content_with_attachments("看图", &attachments);
    retain_submitted_from_content(&content);

    let displays = attachment_displays_from_content(&content);
    assert_eq!(displays.len(), 1);
    let atom = crate::kit::atoms::RETAINED_ATTACHMENTS.state();
    assert!(
        atom.read().get(displays[0].payload_id).is_some(),
        "投递路径的保留身份与展示记录一致"
    );
    // 纯文本内容不保留任何载荷。
    retain_submitted_from_content(&MessageContent::text("无图"));
    assert_eq!(atom.read().len(), 1, "文本内容不引入保留条目");
    atom.write().clear();
}

/// 图片-only 提交（空文本 + 附件）不得产出空 text 块——provider 会拒收空
/// text block；此时 content 只应含 image 块。
#[test]
fn test_attachment_only_content_has_no_empty_text_block() {
    let attachments = vec![PendingAttachment::image("image/png", "AQID")];
    let content = content_with_attachments("", &attachments);

    let blocks = content.content_blocks();
    assert_eq!(blocks.len(), 1, "空文本不得产出 text 块: {blocks:?}");
    assert!(matches!(blocks[0], ContentBlock::Image { .. }));
    assert_eq!(
        attachments_from_content(&content)[0].base64_data,
        "AQID",
        "附件数据须完整可恢复"
    );
}

/// 无附件时 content 形态与旧路径逐字一致（纯文本，不是单元素 block 数组）。
#[test]
fn test_content_without_attachments_stays_plain_text() {
    let content = content_with_attachments("hello", &[]);
    assert_eq!(content, MessageContent::text("hello"));
}

/// 投递路径的附件展示记录与提交路径同源：字节数取自图片块载荷长度。
#[test]
fn test_attachment_displays_from_content_matches_submit_path() {
    let attachments = vec![
        PendingAttachment::image("image/png", "AQID"),
        PendingAttachment::image("image/png", "AQI="),
    ];
    let content = content_with_attachments("两张图", &attachments);

    let from_submit = attachment_displays(&attachments);
    let from_content = attachment_displays_from_content(&content);
    assert_eq!(from_submit, from_content, "两条路径的展示记录必须一致");
    assert_eq!(
        from_submit.iter().map(|a| a.bytes).collect::<Vec<_>>(),
        vec![3, 2],
        "字节数 = base64 解码后的载荷长度"
    );

    // 纯文本内容没有附件记录。
    assert!(attachment_displays_from_content(&MessageContent::text("hello")).is_empty());
    assert!(attachment_displays(&[]).is_empty());
}
