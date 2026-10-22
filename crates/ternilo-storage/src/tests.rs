use super::*;

#[tokio::test]
#[ignore = "requires TERNILO_STORAGE_TEST_DATABASE_URL for a disposable PostgreSQL database"]
async fn postgres_cancelled_begin_does_not_leak_read_only_state_to_the_next_request() {
    const BEGIN: &str = "BEGIN READ ONLY; SELECT pg_advisory_xact_lock(730987541)";
    let url = std::env::var("TERNILO_STORAGE_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_storage_test"));
    let database = Database::connect(&url, 1).await.unwrap();
    let observer = Database::connect(&url, 1).await.unwrap();
    sqlx::query("CREATE TABLE cancelled_begin_marker (value BIGINT)")
        .execute(database.pool())
        .await
        .unwrap();

    // Hold BEGIN's response after PostgreSQL has entered the read-only transaction.
    sqlx::query("SELECT pg_advisory_lock(730987541)")
        .execute(observer.pool())
        .await
        .unwrap();
    let interrupted = tokio::spawn({
        let database = database.clone();
        async move { database.begin_postgres(BEGIN).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let started: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pg_stat_activity WHERE datname=current_database() AND query=$1 AND wait_event='advisory'")
                .bind(BEGIN).fetch_one(observer.pool()).await.unwrap();
            if started == 1 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("the interrupted BEGIN must reach PostgreSQL");
    interrupted.abort();
    assert!(interrupted.await.unwrap_err().is_cancelled());
    sqlx::query("SELECT pg_advisory_unlock(730987541)")
        .execute(observer.pool())
        .await
        .unwrap();

    let mut next = database.begin().await.unwrap();
    sqlx::query("INSERT INTO cancelled_begin_marker VALUES (1)")
        .execute(&mut *next)
        .await
        .expect("a cancelled read must not poison the next writer");
    next.commit().await.unwrap();
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cancelled_begin_marker")
        .fetch_one(observer.pool())
        .await
        .unwrap();
    assert_eq!(stored, 1, "the new request must commit independently");
    database.close().await;
    observer.close().await;
}
