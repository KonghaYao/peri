use super::*;

#[test]
fn callback_rejects_wrong_state_path_duplicates_and_client_switch() {
    for url in [
        "http://127.0.0.1:7000/auth/callback?state=wrong&code=code&client_id=issued",
        "http://127.0.0.1:7000/wrong?state=state&code=code&client_id=issued",
        "http://127.0.0.1:7000/auth/callback?state=state&state=state&code=code&client_id=issued",
        "http://127.0.0.1:7000/auth/callback?state=state&code=code&client_id=other",
        "http://127.0.0.1:7000/auth/callback?state=state&code=code&error=access_denied",
    ] {
        assert!(
            matches!(
                validate_callback(&Url::parse(url).unwrap(), "state", Some("issued")),
                Err(AuthError::InvalidCallback)
            ),
            "{url}"
        );
    }
}

#[test]
fn initial_callback_requires_issued_client_and_returning_callback_can_omit_it() {
    let url = Url::parse("http://127.0.0.1:7000/auth/callback?state=state&code=code").unwrap();
    assert!(matches!(
        validate_callback(&url, "state", None),
        Err(AuthError::InvalidCallback)
    ));
    assert_eq!(
        validate_callback(&url, "state", Some("issued"))
            .unwrap()
            .client_id,
        "issued"
    );
    let mut initial = url;
    initial
        .query_pairs_mut()
        .append_pair("client_id", "dynamic_agent_client");
    assert!(matches!(
        validate_callback(&initial, "state", None),
        Err(AuthError::InvalidCallback)
    ));
}

#[test]
fn denial_only_applies_after_state_validation() {
    let url =
        Url::parse("http://127.0.0.1:7000/auth/callback?state=state&error=access_denied").unwrap();
    assert!(matches!(
        validate_callback(&url, "state", None),
        Err(AuthError::AccessDenied)
    ));
    assert!(matches!(
        validate_callback(&url, "wrong", None),
        Err(AuthError::InvalidCallback)
    ));
}

#[tokio::test]
async fn issuer_error_callback_preserves_diagnosis_and_finishes_immediately() {
    let (attempt, pending) = LoginAttempt::begin(
        &OAuth::production().unwrap(),
        StoredAccount::new("synthetic-host".into()),
    )
    .await
    .unwrap();
    let fields: HashMap<_, _> = Url::parse(pending.authorization_url())
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let mut callback = Url::parse(&attempt.redirect).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &fields["state"])
        .append_pair("error", "server_error")
        .append_pair("error_description", "synthetic issuer failure");
    let cancellation = CancellationToken::new();
    let (result, response) = peri_time::timeout(Duration::from_secs(5), async {
        tokio::join!(attempt.receive(&cancellation), reqwest::get(callback))
    })
    .await
    .unwrap();
    assert_eq!(response.unwrap().status(), 400);
    assert!(matches!(result, Err(AuthError::OAuth { code, description })
        if code == "server_error" && description == "synthetic issuer failure"));
}

#[tokio::test]
async fn loopback_listener_rejects_wrong_host_then_accepts_bound_callback() {
    let (attempt, pending) = LoginAttempt::begin(
        &OAuth::production().unwrap(),
        StoredAccount::new("urn:uuid:00000000-0000-4000-8000-000000000001".into()),
    )
    .await
    .unwrap();
    let authorize = Url::parse(pending.authorization_url()).unwrap();
    let fields: HashMap<_, _> = authorize.query_pairs().into_owned().collect();
    assert_eq!(fields["client_id"], "dynamic_agent_client");
    assert_eq!(fields["code_challenge_method"], "S256");
    assert_ne!(fields["code_challenge"], attempt.verifier);
    let mut callback = Url::parse(&attempt.redirect).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &fields["state"])
        .append_pair("code", "synthetic-code")
        .append_pair("client_id", "issued-client");
    let cancellation = CancellationToken::new();
    let receiving = attempt.receive(&cancellation);
    let sending = async {
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(callback.clone())
                .header("Host", "evil.test")
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(client.get(callback).send().await.unwrap().status(), 200);
    };
    let (result, ()) = tokio::join!(receiving, sending);
    assert_eq!(result.unwrap().client_id, "issued-client");
}

#[tokio::test]
async fn cancelling_callback_does_not_wait_for_browser() {
    let (attempt, _) = LoginAttempt::begin(
        &OAuth::production().unwrap(),
        StoredAccount::new("test-host".into()),
    )
    .await
    .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        attempt.receive(&cancellation).await,
        Err(AuthError::Cancelled)
    ));
}
