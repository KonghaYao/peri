use super::*;

#[test]
fn transaction_request_keeps_parameters_separate_and_lossless() {
    let sql = "SELECT $1::bigint, $2::text, $3::text, $4::bytea, $5::float8";
    let spec = StatementSpec::new(
        sql,
        vec![
            Value::Integer(i64::MAX),
            Value::Text("错'误".into()),
            Value::Null,
            Value::Blob(vec![0, 255]),
            Value::Real(1.25),
        ],
    );
    let expected = json!({ "query": sql, "params": ["9223372036854775807", "错'误", null, "\\x00ff", "1.25"] });
    let write = request_body(std::slice::from_ref(&spec), true);
    assert_eq!(write["queries"][0]["query"], PIN_TRANSACTION_SCHEMA);
    assert_eq!(write["queries"][1]["query"], LOCK_STORE);
    assert_eq!(write["queries"][2], expected);
    let read = request_body(&[spec], false);
    assert_eq!(read["queries"].as_array().unwrap().len(), 2);
    assert_eq!(read["queries"][1], expected);
}

#[test]
fn raw_array_results_preserve_bigint_null_bytes_bool_and_dml_counts() {
    let prefix = json!({"command":"SELECT","rowCount":1,"fields":[],"rows":[]});
    let data = json!({"command":"SELECT","rowCount":1,
        "fields":[{"dataTypeID":20},{"dataTypeID":16},{"dataTypeID":17},{"dataTypeID":25},{"dataTypeID":16}],
        "rows":[["9223372036854775807","f","\\x00ff",null,"t"]]});
    let reply = json!({ "results": [prefix, data] }).to_string();
    let mut read = results(&reply, 1, false).unwrap();
    assert_eq!(
        read.pop().unwrap().values().unwrap(),
        vec![vec![
            Value::Integer(i64::MAX),
            Value::Integer(0),
            Value::Blob(vec![0, 255]),
            Value::Null,
            Value::Integer(1)
        ]]
    );
    let ddl = json!({"command":"CREATE","rowCount":null,"fields":[],"rows":[]});
    let insert = json!({"command":"INSERT","rowCount":1,"fields":[],"rows":[]});
    let reply = json!({ "results": [prefix, prefix, ddl, insert] }).to_string();
    let counts = results(&reply, 2, true)
        .unwrap()
        .into_iter()
        .map(QueryResult::affected_rows)
        .collect::<TransportResult<Vec<_>>>()
        .unwrap();
    assert_eq!(counts, [0, 1]);
    assert!(results(&reply, 3, true).is_err());
}

#[test]
fn proxy_errors_classify_known_rollbacks_without_claiming_a_lost_commit() {
    let statements = [
        StatementSpec::bare("INSERT INTO peri_op_ledger (operation_id) VALUES ($1)"),
        StatementSpec::bare("INSERT INTO messages (message_id) VALUES ($1)"),
    ];
    let unique = r#"{"code":"23505","table":"peri_op_ledger","constraint":"peri_op_ledger_pkey","detail":"secret"}"#;
    assert!(matches!(
        server_failure(400, unique, &statements),
        TransportError::RolledBack {
            unique: true,
            index: Some(0),
            class: RemoteFailureClass::Constraint
        }
    ));
    assert!(matches!(
        server_failure(400, r#"{"code":"08007"}"#, &statements),
        TransportError::Uncertain(_)
    ));
    assert!(matches!(
        server_failure(502, "", &statements),
        TransportError::Uncertain(_)
    ));
    assert!(matches!(
        server_failure(403, "", &statements),
        TransportError::Before(RemoteFailureClass::AuthRejected)
    ));
}

#[test]
fn default_route_only_sends_credentials_to_neons_sql_proxy() {
    let endpoint = |dsn| neon_endpoint(&url::Url::parse(dsn).unwrap());
    assert_eq!(
        endpoint("postgres://user:pass@ep-main.us-east-1.aws.neon.tech/db").as_deref(),
        Some("https://api.us-east-1.aws.neon.tech/sql")
    );
    assert!(endpoint("postgres://user@db.neon.tech.other.example/db").is_none());
    assert!(endpoint("postgres://user@db.example.com/db").is_none());
}

#[test]
fn only_the_identity_insert_can_report_a_competing_initializer() {
    use crate::sessions::remote::schema;
    let plan = schema::initialization_plan(&schema::StoreId::mint(), "now");
    let occupied =
        r#"{"code":"23505","table":"peri_store_meta","constraint":"peri_store_meta_pkey"}"#;
    assert!(matches!(
        server_failure(400, occupied, &plan),
        TransportError::RolledBack {
            unique: true,
            index: Some(schema::META_INSERT_INDEX),
            class: RemoteFailureClass::Constraint
        }
    ));
    let collision = r#"{"code":"23502","table":"peri_store_meta","column":"schema_version"}"#;
    assert!(matches!(
        server_failure(400, collision, &plan),
        TransportError::RolledBack {
            unique: false,
            index: None,
            class: RemoteFailureClass::Constraint
        }
    ));
}
