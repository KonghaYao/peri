//! Full lifecycle regressions with real SQLite and model-response fixtures.

use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::compact_handoff::CompactHandoff;
use peri_model::{Model, ModelCapabilities, ModelResponse, ModelResult, ModelStream, StopReason};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex;

struct FixtureModel {
    responses: Mutex<VecDeque<(String, StopReason)>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl FixtureModel {
    fn new(responses: impl IntoIterator<Item = (String, StopReason)>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }
    fn one(text: String) -> Self {
        Self::new([(text, StopReason::EndTurn)])
    }
}

#[async_trait::async_trait]
impl Model for FixtureModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelStream> {
        unreachable!()
    }
    async fn complete(
        &self,
        request: ModelRequest,
        _: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.requests.lock().unwrap().push(request);
        let (text, stop) = self.responses.lock().unwrap().pop_front().unwrap();
        ModelResponse::new(ModelMessage::assistant_text(text), stop, None, None)
    }
}

fn summary(add: serde_json::Value, retire: serde_json::Value) -> String {
    format!("<summary><continuity_delta>{}</continuity_delta>\nPreviously read the report; its body will be archived. Next: run the remaining checks, results unknown.</summary>", json!({"add":add,"retire":retire}))
}

fn first_summary() -> String {
    summary(
        json!([
            {"kind":"acceptance","source":"s1","quote":"全部场景 p99 < 100 ms；不接受部分通过。"},
            {"kind":"entity","source":"s1","quote":"Astra 是裁判。"},
            {"kind":"authorization","source":"s1","quote":"只生成 patch，不自动合并。"},
        ]),
        json!([]),
    )
}

async fn initial(bound: &TestSession) -> MessageTranscript {
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    transcript.append(BaseMessage::human(
        "全部场景 p99 < 100 ms；不接受部分通过。Astra 是裁判。只生成 patch，不自动合并。",
    ));
    transcript.flush_persistence().await.unwrap();
    transcript
}

fn state(transcript: &MessageTranscript) -> (MessageId, CompactHandoff) {
    let active: Vec<_> = transcript
        .entries()
        .iter()
        .filter(|entry| !transcript.is_excluded(entry.id()))
        .filter_map(|entry| {
            transcript
                .projection_ref(entry.id())
                .and_then(|p| p.compact_handoff.clone())
                .map(|state| (entry.id(), state))
        })
        .collect();
    assert_eq!(active.len(), 1);
    active.into_iter().next().unwrap()
}

async fn compact(
    transcript: &mut MessageTranscript,
    model: &FixtureModel,
) -> super::super::CompactResult {
    full_compact_inner(
        transcript,
        Some(model),
        &CompactConfig::default(),
        "/nonexistent-worker-root",
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn full_handoff_survives_thirty_compactions_and_fresh_sqlite_loads() {
    let bound = TestSession::open().await;
    let mut transcript = initial(&bound).await;
    let original_id = transcript.entries()[0].id();
    compact(&mut transcript, &FixtureModel::one(first_summary())).await;
    let expected = state(&transcript).1;
    assert_eq!(expected.entries.len(), 3);
    for round in 1..30 {
        // A fresh resource handle decodes SQLite metadata; no hot transcript state is reused.
        let fresh = bound.read_only_resources().await;
        let active = fresh.load_active_snapshot(&bound.thread_id).await.unwrap();
        transcript = MessageTranscript::new()
            .with_own_payloads(active.payloads)
            .with_persistence(bound.resources(), bound.thread_id.clone());
        transcript.set_flags_batch(active.flags);
        assert_eq!(state(&transcript).1, expected, "reload {round}");
        transcript.append(BaseMessage::human(format!(
            "继续第 {round} 轮，验收仍待完成"
        )));
        // Alternate omitted entries and entirely omitted deltas: neither can erase state.
        let text = if round % 2 == 0 {
            summary(json!([]), json!([]))
        } else {
            "<summary>Continue checking recent work.</summary>".into()
        };
        let model = FixtureModel::one(text);
        let result = compact(&mut transcript, &model).await;
        assert_eq!(state(&transcript).1, expected, "compact {round}");
        let rendered = result.summary.unwrap();
        for exact in [
            "p99 < 100 ms",
            "不接受部分通过",
            "Astra 是裁判",
            "不自动合并",
        ] {
            assert!(rendered.contains(exact));
        }
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].tools.is_empty());
    }
    let saved = bound
        .resources()
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(saved.flags[&original_id].excluded);
    assert!(saved
        .payloads
        .iter()
        .any(|payload| payload.id() == original_id));
    let states: Vec<_> = saved
        .flags
        .values()
        .filter_map(|f| f.projection.as_ref()?.compact_handoff.as_ref())
        .collect();
    assert_eq!(
        states.len(),
        30,
        "archived summaries retain their historical continuity state for rewind"
    );
    assert!(states.into_iter().all(|state| *state == expected));
}

#[tokio::test]
async fn full_handoff_invalid_quote_cannot_commit_exclusions_or_summary() {
    let bound = TestSession::open().await;
    let mut transcript = initial(&bound).await;
    let before = serde_json::to_value(transcript.visible_messages()).unwrap();
    let model = FixtureModel::one(summary(
        json!([{"kind":"acceptance","source":"s1","quote":"p99 < 10 ms"}]),
        json!([]),
    ));
    let error = full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("quote does not match"));
    assert_eq!(
        serde_json::to_value(transcript.visible_messages()).unwrap(),
        before
    );
    let saved = bound
        .resources()
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(saved.payloads.len(), 1);
    assert!(saved
        .flags
        .values()
        .all(|flags| !flags.excluded && flags.projection.is_none()));
}

#[tokio::test]
async fn full_handoff_rewind_restores_prior_contract_instead_of_new_revision() {
    let bound = TestSession::open().await;
    let mut transcript = initial(&bound).await;
    compact(&mut transcript, &FixtureModel::one(first_summary())).await;
    let (first_id, original) = state(&transcript);
    transcript.append(BaseMessage::human(
        "修改验收：全部场景 p99 < 80 ms；不接受部分通过。",
    ));
    let model = FixtureModel::one(summary(
        json!([{"kind":"acceptance","source":"s2","quote":"全部场景 p99 < 80 ms；不接受部分通过。"}]),
        json!([{"id":original.entries[0].id,"source":"s2","quote":"修改验收：全部场景 p99 < 80 ms；不接受部分通过。"}]),
    ));
    compact(&mut transcript, &model).await;
    let changed = state(&transcript).1;
    assert!(!changed
        .entries
        .iter()
        .any(|entry| entry.evidence.quote.contains("100 ms")));
    let saved = bound
        .resources()
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(
        saved.flags[&first_id]
            .projection
            .as_ref()
            .unwrap()
            .compact_handoff
            .as_ref(),
        Some(&original)
    );
    let mut rewind = MessageTranscript::new()
        .with_own_payloads(saved.payloads)
        .with_persistence(bound.resources(), bound.thread_id.clone());
    rewind.set_flags_batch(saved.flags);
    rewind.rewind_to(first_id).await.unwrap();
    assert_eq!(state(&rewind).1, original);
    compact(
        &mut rewind,
        &FixtureModel::one(summary(json!([]), json!([]))),
    )
    .await;
    assert_eq!(state(&rewind).1, original);
}

#[tokio::test]
async fn full_handoff_delta_split_across_responses_uses_existing_recovery_budget() {
    let bound = TestSession::open().await;
    let mut transcript = initial(&bound).await;
    let text = first_summary();
    let split = text.find("acceptance").unwrap() + 3;
    let model = FixtureModel::new([
        (text[..split].into(), StopReason::MaxTokens),
        (text[split..].into(), StopReason::EndTurn),
    ]);
    compact(&mut transcript, &model).await;
    assert_eq!(state(&transcript).1.entries.len(), 3);
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let serialized = serde_json::to_string(&requests[1]).unwrap();
    assert_eq!(serialized.matches("Compact source index").count(), 1);
    assert!(requests.iter().all(|r| r.tools.is_empty()));
}

#[tokio::test]
async fn full_handoff_failed_store_does_not_publish_candidate_state() {
    let bound = TestSession::open().await;
    let transcript = initial(&bound).await;
    let before = serde_json::to_value(transcript.visible_messages()).unwrap();
    let saved = bound
        .resources()
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new()
        .with_own_payloads(saved.payloads)
        .with_persistence(bound.read_only_resources().await, bound.thread_id.clone());
    transcript.set_flags_batch(saved.flags);
    let model = FixtureModel::one(first_summary());
    assert!(full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp"
    )
    .await
    .is_err());
    assert_eq!(
        serde_json::to_value(transcript.visible_messages()).unwrap(),
        before
    );
    let saved = bound
        .resources()
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(saved.payloads.len(), 1);
    assert!(saved
        .flags
        .values()
        .all(|f| !f.excluded && f.projection.is_none()));
}
