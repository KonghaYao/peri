use super::schema::{
    self, StoreId, StoreIdentityRead, StoreSnapshot, REMOTE_SCHEMA_VERSION, STORE_CONTRACT,
};
use super::sql::Value;

#[test]
fn only_a_current_postgres_contract_is_accepted() {
    let snapshot = StoreSnapshot {
        store_id: StoreId::mint(),
        schema_version: REMOTE_SCHEMA_VERSION,
        contract: STORE_CONTRACT.to_owned(),
    };
    assert!(snapshot.matches_build());
    assert!(!StoreSnapshot {
        contract: "other".to_owned(),
        ..snapshot.clone()
    }
    .matches_build());
    assert!(!StoreSnapshot {
        schema_version: REMOTE_SCHEMA_VERSION + 1,
        ..snapshot
    }
    .matches_build());
}

#[test]
fn metadata_is_either_absent_or_has_a_complete_identity() {
    assert_eq!(
        schema::interpret_identity_read(false, None),
        StoreIdentityRead::Uninitialized
    );
    assert_eq!(
        schema::interpret_identity_read(true, None),
        StoreIdentityRead::Malformed
    );
    let row = [
        Value::Integer(1),
        Value::Text(String::new()),
        Value::Text(STORE_CONTRACT.to_owned()),
    ];
    assert_eq!(
        schema::interpret_identity_read(true, Some(&row)),
        StoreIdentityRead::Malformed
    );
}

/// Pin the emitted PostgreSQL schema independently of the live catalog check.
/// Formatting and the order of independent object definitions do not matter.
#[test]
fn postgres_ddl_matches_its_declared_schema_version() {
    use sha2::{Digest, Sha256};

    let mut definitions: Vec<String> = schema::initialization_plan(&StoreId::mint(), "test")
        .into_iter()
        .map(|statement| statement.sql)
        .filter(|sql| {
            matches!(
                sql.split_whitespace().next(),
                Some("CREATE" | "ALTER" | "DROP")
            )
        })
        .map(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    definitions.sort_unstable();
    let digest: String = Sha256::digest(definitions.join("\n").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    assert_eq!(
        (REMOTE_SCHEMA_VERSION, digest.as_str()),
        (1, "ebeb87450e2de8857815b069057fe2ccaa4054e60ab94517ec5cee560d1fdd57"),
        "PostgreSQL DDL changed: review the upgrade path and bump the schema version before recording the new fingerprint"
    );
}
