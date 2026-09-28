use super::*;

pub(super) async fn assert_restricted_runtime(store: &ControlStore) {
    let row = sqlx::query("SELECT CAST(rolsuper AS INTEGER) AS superuser,CAST(rolbypassrls AS INTEGER) AS bypass FROM pg_roles WHERE rolname=current_user")
        .fetch_one(store.database().pool()).await.unwrap();
    assert_eq!(row.get::<i64, _>("superuser"), 0);
    assert_eq!(row.get::<i64, _>("bypass"), 0);
    for table in [
        "control_model_devices",
        "control_model_device_authorizations",
        "control_model_requests",
        "control_model_attempts",
    ] {
        let owner: i64 = sqlx::query_scalar("SELECT CAST(tableowner=current_user AS INTEGER) FROM pg_tables WHERE schemaname='public' AND tablename=$1")
            .bind(table).fetch_one(store.database().pool()).await.unwrap();
        assert_eq!(owner, 0, "runtime owns {table}");
        let active: i64 =
            sqlx::query_scalar("SELECT CAST(row_security_active(CAST($1 AS TEXT)) AS INTEGER)")
                .bind(table)
                .fetch_one(store.database().pool())
                .await
                .unwrap();
        assert_eq!(active, 1, "RLS inactive for {table}");
    }
    println!(
        "runtime is non-superuser, has no bypass RLS, owns no model tables; RLS active on all four tables"
    );
}

pub(super) async fn assert_scopes_cleared(store: &ControlStore) {
    let mut connections = Vec::new();
    for _ in 0..8 {
        connections.push(store.database().pool().acquire().await.unwrap());
    }
    for connection in &mut connections {
        for setting in [
            "ternilo.model_service",
            "ternilo.tenant_id",
            "ternilo.user_id",
        ] {
            let value: String = sqlx::query_scalar("SELECT COALESCE(current_setting($1,true),'')")
                .bind(setting)
                .fetch_one(&mut **connection)
                .await
                .unwrap();
            assert_eq!(value, "", "{setting} leaked through the runtime pool");
        }
        let devices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_devices")
            .fetch_one(&mut **connection)
            .await
            .unwrap();
        let requests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_requests")
            .fetch_one(&mut **connection)
            .await
            .unwrap();
        assert_eq!((devices, requests), (0, 0));
    }
}

pub(super) async fn assert_transaction_scope_lifecycle(store: &ControlStore) {
    for commit in [true, false] {
        let mut transaction = store.model_transaction().await.unwrap();
        let devices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_devices")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
        assert!(
            devices > 0,
            "model scope reveals actual authorized device rows"
        );
        if commit {
            transaction.commit().await.unwrap();
        } else {
            transaction.rollback().await.unwrap();
        }
        assert_scopes_cleared(store).await;
    }
    println!(
        "model/user/tenant scopes clear on all eight pool connections after commit and rollback"
    );
}

pub(super) async fn assert_schema_rejection(admin: &sqlx::PgPool, url: &str, runtime: &str) {
    let original: i64 =
        sqlx::query_scalar("SELECT version FROM ternilo_schema WHERE component='control'")
            .fetch_one(admin)
            .await
            .unwrap();
    assert_eq!(original, 14);
    for previous in [6, 13] {
        sqlx::query("UPDATE ternilo_schema SET version=$1 WHERE component='control'")
            .bind(previous)
            .execute(admin)
            .await
            .unwrap();
        for (database_url, migration) in [(url, None), (runtime, Some(url))] {
            let error =
                ControlStore::connect(database_url, migration, SecretCipher::from_key([71; 32]), 1)
                    .await
                    .err()
                    .unwrap();
            assert_eq!(error.code, ErrorCode::InvalidInput);
            assert_eq!(
                error.message,
                format!(
                    "database schema for \"control\" is version {previous}, but this build requires version 14"
                )
            );
        }
        let unchanged: i64 =
            sqlx::query_scalar("SELECT version FROM ternilo_schema WHERE component='control'")
                .fetch_one(admin)
                .await
                .unwrap();
        assert_eq!(unchanged, previous, "no implicit historical migration");
    }
    sqlx::query("UPDATE ternilo_schema SET version=$1 WHERE component='control'")
        .bind(original)
        .execute(admin)
        .await
        .unwrap();
    println!(
        "schema 14 installed; versions 6 and 13 rejected without mutation for owner and runtime entrypoints"
    );
}
