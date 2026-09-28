/// Create runtime identities before production schema initialization, without granting application objects in tests.
pub async fn prepare_role(admin: &sqlx::PgPool, role: &str, password: &str) {
    assert!(
        role.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    );
    assert!(!password.contains('\''));
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
                CREATE ROLE ternilo_runtime NOLOGIN;
            END IF;
        END $$;
        DROP ROLE IF EXISTS {role};
        CREATE ROLE {role} LOGIN PASSWORD '{password}';
        GRANT ternilo_runtime TO {role};"
    )))
    .execute(admin)
    .await
    .unwrap();
    grant_schema_usage(admin, role).await;
}

pub async fn grant_schema_usage(admin: &sqlx::PgPool, role: &str) {
    assert!(
        role.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "REVOKE CREATE ON SCHEMA public FROM PUBLIC; GRANT USAGE ON SCHEMA public TO {role}"
    )))
    .execute(admin)
    .await
    .unwrap();
}
