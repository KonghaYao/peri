use super::*;

async fn create_test_session(harness: &Harness) {
    let (status, _, body) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({"logicalId":"agent-01"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn list(harness: &Harness, query: &str) -> (StatusCode, Value) {
    let uri = format!("/v1/agents/sessions/agent-01/items?{query}");
    let (status, _, body) = send(&harness.router, Method::GET, &uri, None).await;
    (status, body)
}

async fn seed_internal_pages(harness: &Harness) {
    create_test_session(harness).await;
    append_row(harness, 1, "itim_opener", "user", json!({})).await;
    for seq in 2..=7 {
        match seq {
            3 => {
                append_item(harness, seq, "itim_approval", ItemKind::Approval, json!({})).await;
            }
            4 | 6 => {
                let id = if seq == 4 {
                    "itim_answer"
                } else {
                    "itim_followup"
                };
                append_row(harness, seq, id, "assistant", json!({"complete":true})).await;
            }
            _ => {
                append_item(
                    harness,
                    seq,
                    &format!("itim_receipt_{seq}"),
                    ItemKind::Receipt,
                    json!({}),
                )
                .await;
            }
        }
    }
}

#[tokio::test]
async fn internal_only_pages_extend_the_raw_window_to_the_first_projection() {
    let harness = harness();
    seed_internal_pages(&harness).await;
    let (status, page) = list(&harness, "order=asc&limit=2&after=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&page), [2, 3, 4, 5]);
    assert_eq!(data_ids(&page), ["itim_answer"]);
    assert_eq!(page["data"][0]["content"][0]["text"], "assistant-4");
    assert_eq!(page["has_more"], true);
    assert_eq!(page["hasMore"], true);
    assert_eq!(page["nextCursor"], 5);

    let (status, page) = list(&harness, "order=asc&limit=2&afterSeq=5").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&page), [6, 7]);
    assert_eq!(data_ids(&page), ["itim_followup"]);
    assert_eq!(page["has_more"], false);
    assert_eq!(page["nextCursor"], Value::Null);
}

#[tokio::test]
async fn sdk_cursor_terminates_on_a_confirmed_internal_only_tail() {
    let harness = harness();
    seed_internal_pages(&harness).await;
    let pages = sdk_pages(&harness, "asc", 1).await;
    assert_eq!(pages.len(), 4);
    assert_eq!(data_ids(&pages[0]), ["itim_opener"]);
    assert_eq!(data_ids(&pages[1]), ["itim_answer"]);
    assert_eq!(data_ids(&pages[2]), ["itim_followup"]);
    assert_eq!(seqs(&pages[3]), [7]);
    assert_eq!(pages[3]["data"], json!([]));
    assert_eq!(pages[3]["has_more"], false);
    assert_eq!(pages[3]["nextCursor"], Value::Null);
}

#[tokio::test]
async fn cursor_inside_an_interleaved_item_returns_both_generations_once() {
    let harness = harness();
    create_test_session(&harness).await;
    append_row(&harness, 1, "itim_opener", "user", json!({})).await;
    for (seq, frame) in [
        (2, json!({"delta":true,"text":"hel"})),
        (3, json!({"complete":true,"text":"hel"})),
        (5, json!({"delta":true,"text":"lo"})),
        (6, json!({"complete":true,"text":"lo"})),
    ] {
        if seq == 5 {
            append_item(&harness, 4, "itim_between", ItemKind::Receipt, json!({})).await;
        }
        append_row(&harness, seq, "itim_two_generations", "assistant", frame).await;
    }
    append_row(
        &harness,
        7,
        "itim_later",
        "assistant",
        json!({"complete":true}),
    )
    .await;

    let (status, full) = list(&harness, "order=asc&limit=20").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        data_ids(&full),
        ["itim_opener", "itim_two_generations", "itim_later"]
    );
    let expected = &full["data"][1];
    assert_eq!(expected["content"][0]["text"], "hello");

    let (status, asc) = list(&harness, "order=asc&limit=1&afterSeq=3").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&asc), [2, 3, 4, 5, 6]);
    assert_eq!(data_ids(&asc), ["itim_two_generations"]);
    assert_eq!(&asc["data"][0], expected);
    assert_eq!(asc["nextCursor"], 6);

    let (status, desc) = list(&harness, "order=desc&limit=1&afterSeq=7").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&desc), [6, 5, 3, 2]);
    assert_eq!(data_ids(&desc), ["itim_two_generations"]);
    assert_eq!(&desc["data"][0], expected);
    assert_eq!(desc["nextCursor"], 6);
    let (status, older) = list(
        &harness,
        &format!("order=desc&limit=1&afterSeq={}", desc["nextCursor"]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&older), [4, 1]);
    assert_eq!(data_ids(&older), ["itim_opener"]);
}

#[tokio::test]
async fn later_answer_preserves_text_and_turn_after_many_interleaved_thoughts() {
    let harness = harness();
    create_test_session(&harness).await;
    append_row(&harness, 1, "itim_opener", "user", json!({})).await;
    for index in 0..10 {
        let seq = 2 + 2 * index;
        append_item(
            &harness,
            seq,
            &format!("itim_thought_{index}"),
            ItemKind::Thought,
            json!({"complete":true}),
        )
        .await;
        append_item(
            &harness,
            seq + 1,
            &format!("itim_receipt_{index}"),
            ItemKind::Receipt,
            json!({}),
        )
        .await;
    }
    for index in 0..10 {
        append_item(
            &harness,
            22 + index,
            &format!("itim_thought_{index}"),
            ItemKind::Thought,
            json!({"delta":true}),
        )
        .await;
    }
    append_row(
        &harness,
        32,
        "itim_answer",
        "assistant",
        json!({"complete":true}),
    )
    .await;

    let (status, first) = list(&harness, "order=asc&limit=11").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data_ids(&first), ["itim_opener"]);
    assert_eq!(first["nextCursor"], 21);
    let (status, scanned) = list(
        &harness,
        &format!("order=asc&limit=11&afterSeq={}", first["nextCursor"]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, direct) = list(&harness, "order=asc&limit=11&after=31").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data_ids(&scanned), ["itim_answer"]);
    assert_eq!(scanned["data"][0]["content"][0]["text"], "assistant-32");
    assert_eq!(
        scanned["data"][0]["turn_id"],
        peri_swarm::domain::turn_id_for_item("itim_opener")
    );
    assert_eq!(scanned["data"], direct["data"]);
    assert_eq!(
        seqs(&scanned),
        (2..=20).step_by(2).chain(22..=32).collect::<Vec<_>>()
    );
    assert_eq!(scanned["has_more"], false);
}

fn assert_scan_limit(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "items_scan_limit");
    assert!(body.get("nextCursor").is_none());
    assert!(body.get("items").is_none());
    assert!(body.get("data").is_none());
}

#[tokio::test]
async fn scan_budget_counts_the_first_page_and_does_not_return_a_cursor_on_failure() {
    let harness = harness();
    create_test_session(&harness).await;
    append_row(&harness, 1, "itim_opener", "user", json!({})).await;
    for seq in 2..=101 {
        append_item(
            &harness,
            seq,
            &format!("itim_receipt_{seq}"),
            ItemKind::Receipt,
            json!({}),
        )
        .await;
    }
    append_row(
        &harness,
        102,
        "itim_answer",
        "assistant",
        json!({"complete":true}),
    )
    .await;

    let (status, body) = list(&harness, "order=asc&limit=1&afterSeq=1").await;
    assert_scan_limit(status, &body);
    let (status, page) = list(&harness, "order=asc&limit=1&afterSeq=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data_ids(&page), ["itim_answer"]);
    assert_eq!(seqs(&page), (3..=102).collect::<Vec<_>>());
}

#[tokio::test]
async fn raw_row_budget_rejects_a_whole_page_before_exceeding_the_cap() {
    let harness = harness();
    create_test_session(&harness).await;
    append_row(&harness, 1, "itim_opener", "user", json!({})).await;
    for seq in 2..=4003 {
        match seq {
            2002 => {
                append_row(
                    &harness,
                    seq,
                    "itim_answer",
                    "assistant",
                    json!({"complete":true}),
                )
                .await
            }
            _ => {
                let id = if seq <= 2001 {
                    "itim_at_cap"
                } else {
                    "itim_over_cap"
                };
                append_item(&harness, seq, id, ItemKind::Receipt, json!({})).await;
            }
        }
    }
    // 先读满 2000 行后再读到 answer，或下一组自身有 2001 行，都不能返回残页。
    for after in [1, 2002] {
        let (status, body) = list(&harness, &format!("order=asc&limit=1&afterSeq={after}")).await;
        assert_scan_limit(status, &body);
    }
}

// NODE_PATH 指向已安装 openai@7.60.0 的 node_modules；无需网关或模型凭据。
#[tokio::test]
#[ignore = "requires Node.js and openai@7.60.0"]
async fn openai_sdk_iterates_past_internal_only_pages() {
    let harness = harness();
    seed_internal_pages(&harness).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, harness.router).await.unwrap() });
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/sdk_items_pagination_test.mjs"
    );
    let output = tokio::process::Command::new("node")
        .arg(script)
        .arg(base_url)
        .env("PERI_SWARM_API_KEY", TOKEN)
        .output()
        .await
        .expect("run the SDK pagination probe");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
