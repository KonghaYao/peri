use super::*;

async fn seed_internal_pages(harness: &Harness) {
    let (status, _, _) = send(
        &harness.router,
        Method::POST,
        "/v1/agents/sessions",
        Some(json!({"logicalId":"agent-01"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    append_row(harness, 1, "itim_opener", "user", json!({})).await;
    append_item(harness, 2, "itim_receipt", ItemKind::Receipt, json!({})).await;
    append_item(
        harness,
        3,
        "itim_approval",
        ItemKind::Approval,
        json!({"state":"pending"}),
    )
    .await;
    append_row(
        harness,
        4,
        "itim_answer",
        "assistant",
        json!({"complete":true}),
    )
    .await;
}

#[tokio::test]
async fn internal_only_raw_page_keeps_its_rows_and_cursor_but_projects_the_answer() {
    let harness = harness();
    seed_internal_pages(&harness).await;
    let (status, _, page) = send(
        &harness.router,
        Method::GET,
        "/v1/agents/sessions/agent-01/items?order=asc&limit=2&after=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&page), [2, 3]);
    assert_eq!(page["items"][0]["kind"], "receipt");
    assert_eq!(page["items"][1]["kind"], "approval");
    assert_eq!(page["data"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"][0]["id"], "itim_answer");
    assert_eq!(page["data"][0]["content"][0]["text"], "assistant-4");
    assert_eq!(page["has_more"], true);
    assert_eq!(page["hasMore"], true);
    assert_eq!(page["nextCursor"], 3);
    let uri = format!(
        "/v1/agents/sessions/agent-01/items?order=asc&limit=2&afterSeq={}",
        page["nextCursor"]
    );
    let (status, _, page) = send(&harness.router, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seqs(&page), [4]);
    assert_eq!(data_ids(&page), ["itim_answer"]);
    assert_eq!(page["has_more"], false);
    assert_eq!(page["nextCursor"], Value::Null);
}

#[tokio::test]
async fn sdk_cursor_crosses_internal_only_pages_in_both_orders() {
    let harness = harness();
    seed_internal_pages(&harness).await;
    for (order, first, second) in [
        ("asc", "itim_opener", "itim_answer"),
        ("desc", "itim_answer", "itim_opener"),
    ] {
        let pages = sdk_pages(&harness, order, 1).await;
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0]["data"][0]["id"], first);
        assert_eq!(pages[1]["data"][0]["id"], second);
        assert_eq!(pages[2]["data"], json!([]));
        assert_eq!(pages[2]["has_more"], false);
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
