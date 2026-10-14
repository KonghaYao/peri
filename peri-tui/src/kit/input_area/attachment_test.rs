use super::*;
use crate::kit::atoms::PendingAttachment;
use serial_test::serial;

fn push_png(base64: &str) {
    PENDING_ATTACHMENTS
        .state()
        .write()
        .push(PendingAttachment::image("image/png", base64));
}

/// 文本为空时 Backspace 移除**尾部整张**附件（一次一张），不逐字符删。
#[test]
#[serial]
fn backspace_removes_last_attachment_as_one_unit() {
    crate::kit::atoms::init_atoms();
    let atom = PENDING_ATTACHMENTS.state();
    {
        let mut pending = atom.write();
        pending.clear();
        pending.push(PendingAttachment::image("image/png", "AQID"));
        pending.push(PendingAttachment::image("image/png", "BAUG"));
    }

    assert!(
        remove_pending_attachment(&TextAreaState::default(), AttachmentEdge::Last),
        "空文本 Backspace 应消费为整张移除"
    );
    assert_eq!(atom.read().len(), 1, "每次只移除一张");
    assert_eq!(atom.read()[0].base64_data, "AQID", "移除的是尾部一张");
    atom.write().clear();
}

/// 文本为空时 Delete 从头移除一张——与 Backspace 对称，多张时可分别移除。
#[test]
#[serial]
fn delete_removes_first_attachment_when_text_empty() {
    crate::kit::atoms::init_atoms();
    let atom = PENDING_ATTACHMENTS.state();
    {
        let mut pending = atom.write();
        pending.clear();
        pending.push(PendingAttachment::image("image/png", "AQID"));
        pending.push(PendingAttachment::image("image/png", "BAUG"));
    }

    assert!(remove_pending_attachment(
        &TextAreaState::default(),
        AttachmentEdge::First
    ));
    let remaining = atom.read();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].base64_data, "BAUG", "移除的是头部一张");
    drop(remaining);
    atom.write().clear();
}

/// 逐张移除：连续按键按边缘依次收敛，不清空整批。
#[test]
#[serial]
fn repeated_removal_drains_one_attachment_per_key() {
    crate::kit::atoms::init_atoms();
    let atom = PENDING_ATTACHMENTS.state();
    {
        let mut pending = atom.write();
        pending.clear();
    }
    for base64 in ["AQID", "BAUG", "AgME"] {
        push_png(base64);
    }

    assert!(remove_pending_attachment(
        &TextAreaState::default(),
        AttachmentEdge::Last
    ));
    assert_eq!(atom.read().len(), 2);
    assert!(remove_pending_attachment(
        &TextAreaState::default(),
        AttachmentEdge::Last
    ));
    assert_eq!(atom.read().len(), 1);
    assert!(remove_pending_attachment(
        &TextAreaState::default(),
        AttachmentEdge::Last
    ));
    assert!(atom.read().is_empty());
    assert!(
        !remove_pending_attachment(&TextAreaState::default(), AttachmentEdge::Last),
        "无附件时不再消费按键"
    );
}

/// 附件移除只在文本为空时发生——有文本时删除键语义完全不变（逐字符/删词）。
#[test]
#[serial]
fn attachment_removal_requires_empty_text() {
    crate::kit::atoms::init_atoms();
    let atom = PENDING_ATTACHMENTS.state();
    {
        let mut pending = atom.write();
        pending.clear();
        pending.push(PendingAttachment::image("image/png", "AQID"));
    }
    let mut editor = TextAreaState::default();
    editor.insert_str("看一下这张图");

    assert!(!remove_pending_attachment(&editor, AttachmentEdge::Last));
    assert!(!remove_pending_attachment(&editor, AttachmentEdge::First));
    assert_eq!(atom.read().len(), 1, "有文本时附件不动");
    editor.clear();
    assert!(remove_pending_attachment(&editor, AttachmentEdge::Last));
    atom.write().clear();
}

/// 无附件时删除键不做任何事（不吞按键、不产生空操作状态）。
#[test]
#[serial]
fn attachment_removal_noop_without_attachments() {
    crate::kit::atoms::init_atoms();
    PENDING_ATTACHMENTS.state().write().clear();
    assert!(!remove_pending_attachment(
        &TextAreaState::default(),
        AttachmentEdge::Last
    ));
}
