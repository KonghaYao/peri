use super::super::failure::RemoteFailureClass;
use super::super::schema::StoreId;
use super::*;
use crate::sessions::canonical::CREATE_OAUTH_CREDENTIALS_TABLE_SQL;
use crate::sessions::data::SessionDataPort;
use sqlx::{Connection, Row, SqliteConnection};

async fn execute(connection: &mut SqliteConnection, spec: StatementSpec) {
    let mut query = sqlx::query(spec.sql);
    for param in spec.params {
        match param {
            Value::Text(text) => query = query.bind(text),
            _ => panic!("unexpected parameter type"),
        }
    }
    query.execute(connection).await.unwrap();
}

async fn read(connection: &mut SqliteConnection, spec: StatementSpec) -> Vec<Vec<Value>> {
    let mut query = sqlx::query(spec.sql);
    for param in spec.params {
        match param {
            Value::Text(text) => query = query.bind(text),
            _ => panic!("unexpected parameter type"),
        }
    }
    query
        .fetch_all(connection)
        .await
        .unwrap()
        .into_iter()
        .map(|row| vec![Value::Text(row.try_get::<String, _>(0).unwrap())])
        .collect()
}

async fn database() -> SqliteConnection {
    let mut connection = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    execute(
        &mut connection,
        StatementSpec::bare(CREATE_OAUTH_CREDENTIALS_TABLE_SQL),
    )
    .await;
    connection
}

#[test]
fn sql_binds_all_dynamic_values() {
    let principal = "principal'; DROP TABLE mcp_oauth_credentials;--";
    let machine = "machine'";
    let key = "https://secret-endpoint/'?token=secret";
    let payload = r#"{ "access_token": "top-secret", "refresh_token": "opaque", "extra": [1] }"#;
    for sql in [SELECT_OAUTH_CREDENTIAL_SQL, DELETE_OAUTH_CREDENTIAL_SQL] {
        let spec = statement(sql, principal, machine, Some(key));
        assert_eq!(
            spec.params,
            vec![
                Value::Text(principal.into()),
                Value::Text(machine.into()),
                Value::Text(key.into())
            ]
        );
        assert!(!spec.sql.contains(principal));
        assert!(!spec.sql.contains(key));
    }
    for sql in [LIST_OAUTH_CREDENTIALS_SQL, DELETE_ALL_OAUTH_CREDENTIALS_SQL] {
        let spec = statement(sql, principal, machine, None);
        assert_eq!(
            spec.params,
            vec![Value::Text(principal.into()), Value::Text(machine.into())]
        );
    }
    let spec = save_statement(principal, machine, key, payload);
    assert_eq!(spec.sql, UPSERT_OAUTH_CREDENTIAL_SQL);
    assert_eq!(
        &spec.params[..4],
        &[
            Value::Text(principal.into()),
            Value::Text(machine.into()),
            Value::Text(key.into()),
            Value::Text(payload.into())
        ]
    );
    assert_eq!(spec.params.len(), 5);
    match &spec.params[4] {
        Value::Text(timestamp) => {
            chrono::DateTime::parse_from_rfc3339(timestamp).unwrap();
        }
        _ => panic!("timestamp must be text"),
    }
    let debug = format!("{spec:?}");
    for secret in [principal, machine, key, payload, "top-secret"] {
        assert!(!debug.contains(secret));
    }
}

#[tokio::test]
async fn sqlite_roundtrip_preserves_complete_json_and_updates_same_key() {
    let mut connection = database().await;
    let payload =
        r#"{ "access_token": "secret", "refresh_token": "refresh", "unknown": {"nested": true} }"#;
    let key = "server'; DELETE FROM mcp_oauth_credentials;--";
    execute(
        &mut connection,
        save_statement("local", "machine", key, payload),
    )
    .await;
    let rows = read(
        &mut connection,
        statement(SELECT_OAUTH_CREDENTIAL_SQL, "local", "machine", Some(key)),
    )
    .await;
    assert_eq!(decode_load(rows).unwrap().as_deref(), Some(payload));
    execute(
        &mut connection,
        save_statement("local", "machine", key, r#"{"access_token":"changed"}"#),
    )
    .await;
    let rows = read(
        &mut connection,
        statement(LIST_OAUTH_CREDENTIALS_SQL, "local", "machine", None),
    )
    .await;
    assert_eq!(decode_list(rows).unwrap(), vec![key]);
    execute(
        &mut connection,
        statement(DELETE_OAUTH_CREDENTIAL_SQL, "local", "machine", Some(key)),
    )
    .await;
    let rows = read(
        &mut connection,
        statement(SELECT_OAUTH_CREDENTIAL_SQL, "local", "machine", Some(key)),
    )
    .await;
    assert!(decode_load(rows).unwrap().is_none());
    execute(
        &mut connection,
        statement(DELETE_OAUTH_CREDENTIAL_SQL, "local", "machine", Some(key)),
    )
    .await;
}

#[tokio::test]
async fn principal_and_machine_scope_isolate_all_crud() {
    let mut connection = database().await;
    for (principal, machine, payload) in [
        ("local", "first", r#"{"token":"first"}"#),
        ("local", "second", r#"{"token":"second"}"#),
        ("other", "first", r#"{"token":"other"}"#),
    ] {
        execute(
            &mut connection,
            save_statement(principal, machine, "same-key", payload),
        )
        .await;
    }
    execute(
        &mut connection,
        save_statement("local", "first", "another-key", "{}"),
    )
    .await;
    let rows = read(
        &mut connection,
        statement(LIST_OAUTH_CREDENTIALS_SQL, "local", "first", None),
    )
    .await;
    assert_eq!(decode_list(rows).unwrap(), vec!["another-key", "same-key"]);
    execute(
        &mut connection,
        statement(
            DELETE_OAUTH_CREDENTIAL_SQL,
            "local",
            "first",
            Some("same-key"),
        ),
    )
    .await;
    execute(
        &mut connection,
        statement(DELETE_ALL_OAUTH_CREDENTIALS_SQL, "local", "first", None),
    )
    .await;
    let rows = read(
        &mut connection,
        statement(LIST_OAUTH_CREDENTIALS_SQL, "local", "first", None),
    )
    .await;
    assert!(decode_list(rows).unwrap().is_empty());
    for (principal, machine, expected) in [
        ("local", "second", r#"{"token":"second"}"#),
        ("other", "first", r#"{"token":"other"}"#),
    ] {
        let rows = read(
            &mut connection,
            statement(
                SELECT_OAUTH_CREDENTIAL_SQL,
                principal,
                machine,
                Some("same-key"),
            ),
        )
        .await;
        assert_eq!(decode_load(rows).unwrap().as_deref(), Some(expected));
    }
}

#[test]
fn malformed_records_fail_closed() {
    for rows in [
        vec![vec![Value::Null]],
        vec![vec![Value::Integer(1)]],
        vec![vec![Value::Text("{secret".into())]],
        vec![vec![Value::Text("[]".into())]],
        vec![
            vec![Value::Text("{}".into())],
            vec![Value::Text("{}".into())],
        ],
        vec![vec![]],
    ] {
        assert!(matches!(
            decode_load(rows),
            Err(OAuthCredentialError::InvalidData)
        ));
    }
    for row in [
        vec![Value::Null],
        vec![Value::Text("".into())],
        vec![],
        vec![Value::Text("key".into()), Value::Null],
    ] {
        assert!(matches!(
            decode_list(vec![row]),
            Err(OAuthCredentialError::InvalidData)
        ));
    }
}

#[test]
fn ledger_identities_are_unique_and_do_not_expose_payload() {
    let first = mutation(
        "save_oauth_credentials",
        save_statement("local", "machine", "secret-key", r#"{"token":"secret"}"#),
    );
    let second = mutation("save_oauth_credentials", first.effects[0].clone());
    assert_ne!(
        first.identity.operation_id.as_str(),
        second.identity.operation_id.as_str()
    );
    assert_eq!(first.identity.digest, second.identity.digest);
    let changed = mutation(
        "save_oauth_credentials",
        save_statement("local", "machine", "secret-key", r#"{"token":"changed"}"#),
    );
    assert_ne!(first.identity.digest, changed.identity.digest);
    let debug = format!("{:?}", first.identity);
    assert!(!debug.contains("secret"));
}

#[test]
fn uncertain_and_rejected_writes_never_report_success() {
    for outcome in [
        MutationOutcome::Unknown {
            class: RemoteFailureClass::Timeout,
        },
        MutationOutcome::ClosedNeverApplied,
        MutationOutcome::NotApplied {
            class: RemoteFailureClass::ServerError,
            rejected_statement: Some(1),
        },
    ] {
        assert!(matches!(
            write_result(outcome),
            Err(OAuthCredentialError::Unavailable)
        ));
    }
    assert!(matches!(
        write_result(MutationOutcome::NotApplied {
            class: RemoteFailureClass::Readonly,
            rejected_statement: None
        }),
        Err(OAuthCredentialError::ReadOnly)
    ));
    let identity = mutation(
        "clear_oauth_credentials",
        statement(DELETE_OAUTH_CREDENTIAL_SQL, "local", "machine", Some("key")),
    )
    .identity;
    assert!(write_result(MutationOutcome::Applied {
        receipt: identity.receipt,
        replayed: false
    })
    .is_ok());
}

#[test]
fn errors_discard_backend_detail() {
    let detail = "SELECT secret FROM https://endpoint/?token=secret";
    for kind in [
        SessionResourceErrorKind::InvalidInput {
            detail: detail.into(),
        },
        SessionResourceErrorKind::Unavailable {
            detail: detail.into(),
        },
        SessionResourceErrorKind::Corrupt {
            detail: detail.into(),
        },
        SessionResourceErrorKind::ReadOnlyStore,
        SessionResourceErrorKind::PersistenceUncertain { thread_id: None },
    ] {
        let error = storage_error(SessionResourceError::new(kind));
        let exposed = format!(
            "{error} {error:?} {}",
            serde_json::to_string(&error).unwrap()
        );
        for forbidden in ["SELECT", "secret", "https://endpoint"] {
            assert!(!exposed.contains(forbidden));
        }
    }
}

#[tokio::test]
async fn old_readonly_schema_query_does_not_create_table() {
    let mut connection = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    sqlx::query("PRAGMA query_only = ON")
        .execute(&mut connection)
        .await
        .unwrap();
    let spec = statement(SELECT_OAUTH_CREDENTIAL_SQL, "local", "machine", Some("key"));
    assert!(sqlx::query(spec.sql)
        .bind("local")
        .bind("machine")
        .bind("key")
        .fetch_all(&mut connection)
        .await
        .is_err());
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'mcp_oauth_credentials'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(count, 0);
    assert!(matches!(
        storage_error(RemoteFailureClass::ServerError.into_session_resource_error()),
        OAuthCredentialError::Unavailable
    ));
}

#[test]
fn writable_initialization_uses_shared_ddl() {
    let plan = super::super::session_schema::initialization_plan();
    assert!(plan
        .iter()
        .any(|spec| spec.sql == CREATE_OAUTH_CREDENTIALS_TABLE_SQL && spec.params.is_empty()));
}

#[tokio::test]
async fn getter_returns_wrapper_and_invalid_input_is_rejected_without_storage() {
    let data = Arc::new(RemoteSessionData::closed_for_test(StoreId::mint()));
    let port = data.oauth_credentials().unwrap();
    assert!(matches!(
        port.load("").await,
        Err(OAuthCredentialError::InvalidInput)
    ));
    assert!(matches!(
        port.clear(" ").await,
        Err(OAuthCredentialError::InvalidInput)
    ));
    assert!(matches!(
        port.save("key", "not-json secret").await,
        Err(OAuthCredentialError::InvalidData)
    ));
    assert!(matches!(
        port.load("key").await,
        Err(OAuthCredentialError::Unavailable)
    ));
    assert!(matches!(
        port.list().await,
        Err(OAuthCredentialError::Unavailable)
    ));
    assert!(matches!(
        port.clear_all().await,
        Err(OAuthCredentialError::Unavailable)
    ));
}
