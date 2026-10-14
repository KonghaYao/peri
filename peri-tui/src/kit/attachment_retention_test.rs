use super::*;

fn payload(len: usize, seed: u8) -> String {
    // 只关心长度与内容差异，不必是合法 base64（保留层不解码）。
    (0..len).map(|_| char::from(b'a' + seed % 26)).collect()
}

/// 条数上限：超过 [`MAX_RETAINED_ATTACHMENTS`] 时从最旧淘汰。
#[test]
fn evicts_oldest_when_entry_limit_exceeded() {
    let mut retained = RetainedAttachments::default();
    for index in 0..MAX_RETAINED_ATTACHMENTS + 2 {
        retained.retain("image/png", &payload(16, index as u8), index as u64);
    }

    assert_eq!(retained.len(), MAX_RETAINED_ATTACHMENTS);
    assert_eq!(retained.get(0), None, "最旧的两张被淘汰");
    assert_eq!(retained.get(1), None);
    let newest = (MAX_RETAINED_ATTACHMENTS + 1) as u64;
    assert!(retained.get(newest).is_some(), "最新一张保留");
    assert_eq!(retained.total_bytes(), MAX_RETAINED_ATTACHMENTS * 16);
}

/// 字节上限：单张不超限但累计超限时继续从最旧淘汰。
#[test]
fn evicts_oldest_when_byte_limit_exceeded() {
    let mut retained = RetainedAttachments::default();
    let chunk = MAX_RETAINED_BYTES / 3 + 1; // 三张必然超限
    retained.retain("image/png", &payload(chunk, 0), 1);
    retained.retain("image/png", &payload(chunk, 1), 2);
    retained.retain("image/png", &payload(chunk, 2), 3);

    assert!(retained.total_bytes() <= MAX_RETAINED_BYTES);
    assert!(retained.get(1).is_none(), "最旧的被淘汰以满足字节上限");
    assert!(retained.get(3).is_some(), "最新一张保留");
    assert!(retained.len() <= MAX_RETAINED_ATTACHMENTS);
}

/// 单张超过字节上限 → 不保留（发送不受影响，仅该图没有 hover 预览）。
#[test]
fn oversized_single_payload_is_not_retained() {
    let mut retained = RetainedAttachments::default();
    retained.retain("image/png", &payload(MAX_RETAINED_BYTES + 1, 0), 9);
    assert!(retained.is_empty(), "超限单张不进入保留层");
    assert_eq!(retained.total_bytes(), 0);
}

/// 同一载荷重复提交 = 刷新（不重复占用配额，并刷新 LRU 位置）。
#[test]
fn retaining_same_payload_refreshes_lru_position() {
    let mut retained = RetainedAttachments::default();
    for index in 0..MAX_RETAINED_ATTACHMENTS {
        retained.retain("image/png", &payload(8, index as u8), index as u64);
    }
    // 重新提交最旧一张（同 id）→ 移到队尾，随后新增一张时淘汰的应是次旧。
    retained.retain("image/png", &payload(8, 0), 0);
    retained.retain("image/png", &payload(8, 9), 99);

    assert_eq!(retained.len(), MAX_RETAINED_ATTACHMENTS, "同 id 不重复占位");
    assert!(retained.get(0).is_some(), "刷新后仍在保留层");
    assert!(retained.get(1).is_none(), "被淘汰的是次旧条目");
    assert!(retained.get(99).is_some());
}

/// 取回按身份精确匹配；清空释放全部（会话边界）。
#[test]
fn get_matches_identity_and_clear_releases_all() {
    let mut retained = RetainedAttachments::default();
    retained.retain("image/png", &payload(4, 1), 7);
    assert!(retained.get(7).is_some());
    assert!(
        retained.get(8).is_none(),
        "身份不匹配 → None（绝不返回别的图）"
    );

    retained.clear();
    assert!(retained.is_empty());
    assert_eq!(retained.total_bytes(), 0);
    assert!(retained.get(7).is_none(), "清空后不可见");
}
