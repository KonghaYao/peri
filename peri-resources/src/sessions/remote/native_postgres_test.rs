use std::time::Duration;

use super::super::super::postgres_fixture::test_pool;
use super::*;

#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn postgres_binds_typed_and_null_values_through_the_same_prepared_statement() {
    let pg = PgTransport::from_pool(test_pool().await);
    let values = vec![
        Value::Integer(9_007_199_254_740_993),
        Value::Text("bind $1, 'hello'".to_owned()),
        Value::Blob(vec![0, 0xff]),
        Value::Real(1.25),
    ];
    let nulls = vec![Value::Null; values.len()];
    for params in [nulls.clone(), values, nulls] {
        let spec = StatementSpec::new(
            "SELECT $1::bigint, $2::text, $3::bytea, $4::float8, TRUE, 17::int4",
            params.clone(),
        );
        let mut expected = params;
        expected.extend([Value::Integer(1), Value::Integer(17)]);
        assert_eq!(pg.sql_values(&spec).await.unwrap(), vec![expected]);
    }
    pg.close().await.unwrap();
    assert!(pg
        .sql_values(&StatementSpec::bare("SELECT 1"))
        .await
        .is_err());
    pg.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn abandoning_a_blocked_write_rolls_back_before_the_pool_is_reused() {
    let pool = test_pool().await;
    let pg = Arc::new(PgTransport::from_pool(pool.clone()));
    let backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .unwrap();
    // A separately owned transaction controls the exact moment the batch can proceed.
    let observer = PgPool::connect(&std::env::var("PERI_TEST_POSTGRES_URL").unwrap())
        .await
        .unwrap();
    let mut blocking = observer.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1::bigint)")
        .bind(i64::from(backend))
        .execute(&mut *blocking)
        .await
        .unwrap();
    let task = tokio::spawn({
        let pg = Arc::clone(&pg);
        async move {
            pg.managed_batch(vec![
                StatementSpec::bare("INSERT INTO peri_fixture VALUES (42)"),
                StatementSpec::new(
                    "SELECT pg_advisory_xact_lock($1::bigint)",
                    vec![Value::Integer(i64::from(backend))],
                ),
            ])
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND NOT granted)",
            )
            .bind(backend)
            .fetch_one(&observer)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("batch should reach the blocked second statement");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    blocking.rollback().await.unwrap();
    let rows = pg
        .sql_values(&StatementSpec::bare("SELECT COUNT(*) FROM peri_fixture"))
        .await
        .unwrap();
    assert_eq!(rows, vec![vec![Value::Integer(0)]]);
    pg.close().await.unwrap();
    observer.close().await;
}
