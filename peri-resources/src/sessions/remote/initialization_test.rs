//! Exercise the real connection factory and PostgreSQL transaction boundaries.

use super::mutation::{initialization_evidence, BatchFailure, InitializationEvidence, StoreAccess};
use super::schema;
use super::RemoteFailureClass;

#[test]
fn an_unconfirmed_commit_never_reports_a_new_identity() {
    assert_eq!(
        initialization_evidence(&Err(BatchFailure::Unknown {
            class: RemoteFailureClass::Transport
        })),
        InitializationEvidence::Failed(RemoteFailureClass::Transport),
    );
    let mut counts = vec![0; schema::META_INSERT_INDEX + 1];
    assert_eq!(
        initialization_evidence(&Ok(counts.clone())),
        InitializationEvidence::Existing
    );
    counts[schema::META_INSERT_INDEX] = 1;
    assert_eq!(
        initialization_evidence(&Ok(counts)),
        InitializationEvidence::Created
    );
}

#[cfg(not(target_os = "emscripten"))]
#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL and permission to create temporary schemas"]
async fn concurrent_initializers_and_read_only_open_validate_the_same_schema() {
    use super::session_data::{RemoteSessionData, StoreInitialization};
    use super::{RemoteEndpoint, SessionStoreCredential};
    use sqlx::{AssertSqlSafe, PgPool};

    let base = std::env::var("PERI_TEST_POSTGRES_URL").expect("set test database URL");
    let admin = PgPool::connect(&base).await.expect("connect test database");
    let name = format!("peri_init_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE SCHEMA {name}")))
        .execute(&admin)
        .await
        .unwrap();
    let mut uri = url::Url::parse(&base).unwrap();
    uri.query_pairs_mut()
        .append_pair("options[search_path]", &name);
    let endpoint = RemoteEndpoint::parse(uri.as_str()).unwrap();
    let credential = SessionStoreCredential::default();
    let open = |access| {
        RemoteSessionData::open_with_machine(
            &endpoint,
            &credential,
            access,
            Some("test-machine".into()),
        )
    };

    assert!(open(StoreAccess::ReadOnly).await.is_err());
    let (first, second) = tokio::join!(open(StoreAccess::ReadWrite), open(StoreAccess::ReadWrite));
    let (_, first) = first.unwrap();
    let (_, second) = second.unwrap();
    assert!(matches!(
        (first, second),
        (
            StoreInitialization::Existing,
            StoreInitialization::CreatedByThisOpen
        ) | (
            StoreInitialization::CreatedByThisOpen,
            StoreInitialization::Existing
        )
    ));
    let (_, reader) = open(StoreAccess::ReadOnly).await.unwrap();
    assert_eq!(reader, StoreInitialization::Existing);

    sqlx::raw_sql(AssertSqlSafe(format!(
        "DROP INDEX {name}.idx_messages_active"
    )))
    .execute(&admin)
    .await
    .unwrap();
    assert!(open(StoreAccess::ReadOnly).await.is_err());
    assert!(open(StoreAccess::ReadWrite).await.is_err());
    sqlx::raw_sql(AssertSqlSafe(format!("DROP SCHEMA {name} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
}
