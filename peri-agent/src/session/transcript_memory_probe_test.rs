//! S1 原生复现：Full 提交后 transcript 自有正文与容器容量实际收缩。
//!
//! 使用真实临时 SQLite（[`TestSession`]）+ 受控 compaction lifecycle，不调用外部
//! 模型。断言全部是对象级证据：条目/引用消失、容量下降、被排除正文不在热态；
//! 不用 RSS 单独证明释放，也不承诺固定降幅。

use super::*;

use peri_acp_types::store::CompactionChange;

use crate::session::test_resources::TestSession;

#[tokio::test]
async fn full_commit_releases_archived_own_bodies_and_container_capacity() {
    let session = TestSession::open().await;

    // 冻结继承区：Full 不得改动其条目与字节。
    let inherited: Vec<_> = (0..3)
        .map(|index| {
            PersistedPayload::Message(make_human(&format!(
                "inherited-{index}-{}",
                "i".repeat(256)
            )))
        })
        .collect();
    let mut transcript = MessageTranscript::new()
        .with_persistence(session.resources(), session.thread_id.clone())
        .with_ancestor_payloads(inherited);

    // 49 条自有条目：48 条待归档的大正文 + 1 条保留正文。
    let mut archived_ids = Vec::new();
    for index in 0..48 {
        let body = format!("ARCHIVED-{index:02}-{}", "x".repeat(1024));
        archived_ids.push(transcript.append(make_human(&body)));
    }
    let retained_id = transcript.append(make_ai("RETAINED-OWN-BODY"));
    transcript.flush_persistence().await.unwrap();

    let t0 = transcript.memory_snapshot();
    assert!(t0.own_bytes.body >= 48 * 1024, "夹具必须包含真实正文");
    assert_eq!(t0.inherited_entries, 3);

    // 排除前 40 条自有历史，并提交一条摘要（真实 SQLite 事务）。
    let excluded: Vec<_> = archived_ids[..40]
        .iter()
        .map(|id| {
            (
                *id,
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            )
        })
        .collect();
    let summary = make_ai(&format!("FULL-SUMMARY-{}", "s".repeat(128)));
    let summary_id = summary.id();
    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates: excluded,
            appended_messages: vec![summary],
        })
        .await
        .unwrap();

    let t1 = transcript.memory_snapshot();
    println!("T0=[{t0:?}]\nT1=[{t1:?}]");

    // 继承区字节与条目数不变。
    assert_eq!(t1.inherited_entries, t0.inherited_entries);
    assert_eq!(t1.inherited_bytes, t0.inherited_bytes);

    // 自有区条目与正文减少；三个容器都重建为更小容量。
    assert!(t1.own_entries < t0.own_entries);
    assert!(t1.own_bytes.body < t0.own_bytes.body);
    assert!(t1.own_bytes.reserved < t0.own_bytes.reserved);
    assert!(
        t0.own_bytes.body - t1.own_bytes.body >= 40 * 1024,
        "被排除的 40 条正文（>=40 KiB）必须离开 transcript 自有区"
    );
    assert!(
        t1.entries_capacity_bytes < t0.entries_capacity_bytes,
        "entries Vec 必须按保留条目重建，旧容量释放"
    );
    assert!(
        t1.id_index_capacity_bytes < t0.id_index_capacity_bytes,
        "id_index HashMap 必须按保留条目重建"
    );
    assert!(
        t1.flags_capacity_bytes == 0,
        "被释放条目的 flags 不得继续驻留"
    );
    assert_eq!(t1.excluded_entries, 0, "收缩后不存在仍标记 excluded 的条目");

    // 被归档的条目不再被 transcript 的任何容器引用。
    for id in &archived_ids[..40] {
        assert!(
            transcript.get(*id).is_none(),
            "归档条目 {id:?} 必须离开 entries/id_index"
        );
    }
    let remaining_ids: std::collections::HashSet<_> = transcript
        .entries()
        .iter()
        .map(|entry| entry.id())
        .collect();
    for id in &archived_ids[..40] {
        assert!(!remaining_ids.contains(id));
    }

    // active_payloads 只含有效集合；归档正文不可从热态导出。
    let active = transcript.active_payloads();
    for id in &archived_ids[..40] {
        assert!(!active.iter().any(|payload| payload.id() == *id));
    }
    let active_text = active
        .iter()
        .filter_map(|payload| payload.as_message())
        .map(|message| message.content())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !active_text.contains("ARCHIVED-00-"),
        "被排除正文不得出现在 active_payloads"
    );
    assert!(active_text.contains("RETAINED-OWN-BODY"));
    assert!(active_text.contains("FULL-SUMMARY-"));

    // 保留条目与摘要仍是热态条目。
    assert!(transcript.get(retained_id).is_some());
    assert!(transcript.get(summary_id).is_some());

    // 归档正文仍在权威存储中：释放的是热态引用，不是删除历史。
    let stored = session
        .resources()
        .load_session_snapshot(&session.thread_id)
        .await
        .unwrap();
    assert!(
        stored.payloads.iter().any(|payload| payload
            .as_message()
            .is_some_and(|message| message.content().starts_with("ARCHIVED-00-"))),
        "SQLite 中必须保留归档正文"
    );

    transcript.shutdown_persistence();
}
