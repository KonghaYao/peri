use super::*;
use sha2::{Digest, Sha256};
use std::time::Duration;

#[tokio::test]
async fn own_body_inspection_uses_one_snapshot_across_a_concurrent_overwrite() {
    let (store, data, _directory, id) = session_with_inherited(&[], "parent").await;
    let payload = PersistedPayload::Message(BaseMessage::human("visible".repeat(80_000)));
    let expected = peri_acp_types::store::serialize_persisted_payload(&payload).unwrap();
    let entry = payload.id();
    data.append_history(&id, &[payload]).await.unwrap();
    let tail = data
        .history_window_bounds(&id)
        .await
        .unwrap()
        .own_latest
        .unwrap();
    let copy = data.clone();
    let owner = id.clone();
    let fixed_tail = tail.clone();
    let (started, reading) = tokio::sync::oneshot::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let check = tokio::spawn(async move {
        copy.own_history_body_reader()
            .unwrap()
            .inspect(
                &owner,
                entry,
                &fixed_tail,
                &HistoryReadTicket::new("ticket"),
                Box::new(move |body| {
                    let first = body.fill_buf().unwrap();
                    assert_eq!(first.len(), CHUNK_BYTES as usize);
                    let mut copied = Sha256::new();
                    copied.update(first);
                    let len = first.len();
                    body.consume(len);
                    started.send(()).unwrap();
                    resume.recv_timeout(Duration::from_secs(10)).unwrap();
                    loop {
                        let buf = body.fill_buf().unwrap();
                        if buf.is_empty() {
                            break;
                        }
                        assert!(buf.len() <= CHUNK_BYTES as usize);
                        copied.update(buf);
                        let len = buf.len();
                        body.consume(len);
                    }
                    assert_eq!(copied.finalize(), Sha256::digest(expected.as_bytes()));
                    Ok(true)
                }),
            )
            .await
    });
    reading.await.unwrap();
    let hidden = PersistedPayload::Message(BaseMessage::System {
        id: entry,
        content: "private".repeat(80_000).into(),
        child_skill_ceiling: None,
    });
    sqlx::query("UPDATE messages SET content = ?1 WHERE message_id = ?2")
        .bind(peri_acp_types::store::serialize_persisted_payload(&hidden).unwrap())
        .bind(entry.as_uuid().to_string())
        .execute(&store.database.pool)
        .await
        .unwrap();
    release.send(()).unwrap();
    assert!(check.await.unwrap().unwrap());
    let reader = data.own_history_body_reader().unwrap();
    let visible = reader
        .inspect(
            &id,
            entry,
            &tail,
            &HistoryReadTicket::new("ticket"),
            Box::new(|body| {
                Ok(peri_acp_types::history_page::history_body_displayable_to_client(body).unwrap())
            }),
        )
        .await
        .unwrap();
    assert!(
        !visible,
        "a new inspection must observe the changed canonical role"
    );
    let later = PersistedPayload::Message(BaseMessage::human("after fixed tail"));
    let later_id = later.id();
    data.append_history(&id, &[later]).await.unwrap();
    assert!(reader
        .inspect(
            &id,
            later_id,
            &tail,
            &HistoryReadTicket::new("ticket"),
            Box::new(|_| panic!("out-of-snapshot entry reached the inspection")),
        )
        .await
        .is_err());
}
