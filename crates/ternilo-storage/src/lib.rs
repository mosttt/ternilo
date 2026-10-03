#![forbid(unsafe_code)]

mod json;

use std::{str::FromStr, time::Duration};

use sqlx::{
    Any, AnyConnection, AnyPool,
    any::{AnyConnectOptions, AnyPoolOptions},
};
use ternilo_protocol::{HarnessError, TenantId, UserId};

pub use json::Json;
pub type Transaction = sqlx::Transaction<'static, Any>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

/// One server database shared by control, execution, and gateway domains.
#[derive(Clone)]
pub struct Database {
    pool: AnyPool,
    backend: Backend,
}

impl Database {
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self, HarnessError> {
        if max_connections == 0 {
            return Err(HarnessError::invalid(
                "database connection count must be positive",
            ));
        }
        sqlx::any::install_default_drivers();
        let mut options = AnyConnectOptions::from_str(url).map_err(database_error)?;
        let backend = match options.database_url.scheme() {
            "sqlite" => Backend::Sqlite,
            "postgres" | "postgresql" => Backend::Postgres,
            _ => {
                return Err(HarnessError::invalid(
                    "database URL must use sqlite or postgres",
                ));
            }
        };
        let memory = backend == Backend::Sqlite && url.contains(":memory:");
        if backend == Backend::Sqlite
            && !memory
            && !options
                .database_url
                .query_pairs()
                .any(|(key, _)| key == "mode")
        {
            options
                .database_url
                .query_pairs_mut()
                .append_pair("mode", "rwc");
        }
        let pool = AnyPoolOptions::new()
            .max_connections(if memory { 1 } else { max_connections })
            .acquire_timeout(Duration::from_secs(15))
            .after_connect(move |connection, _| Box::pin(async move {
                if backend == Backend::Sqlite {
                    sqlx::raw_sql("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 15000; PRAGMA journal_mode = WAL;")
                        .execute(connection).await?;
                }
                Ok(())
            }))
            .connect_with(options).await.map_err(database_error)?;
        Ok(Self { pool, backend })
    }

    #[must_use]
    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }

    #[must_use]
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// SQLite obtains its write lock before any reads, avoiding snapshot upgrades.
    pub async fn begin(&self) -> Result<Transaction, HarnessError> {
        match self.backend {
            Backend::Sqlite => self.pool.begin_with("BEGIN IMMEDIATE").await,
            Backend::Postgres => self.pool.begin().await,
        }
        .map_err(database_error)
    }

    /// Read snapshots avoid reserving SQLite's sole writer. Do not use for mutations.
    pub async fn begin_read(&self) -> Result<Transaction, HarnessError> {
        self.pool
            .begin_with(match self.backend {
                Backend::Sqlite => "BEGIN DEFERRED",
                Backend::Postgres => "BEGIN READ ONLY",
            })
            .await
            .map_err(database_error)
    }

    pub async fn tenant_read_transaction(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Transaction, HarnessError> {
        let mut transaction = self.begin_read().await?;
        set_tenant_scope(&mut transaction, tenant_id).await?;
        Ok(transaction)
    }

    pub async fn tenant_transaction(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Transaction, HarnessError> {
        let mut transaction = self.begin().await?;
        set_tenant_scope(&mut transaction, tenant_id).await?;
        Ok(transaction)
    }

    pub async fn owner_transaction(
        &self,
        tenant_id: &TenantId,
        user_id: &UserId,
    ) -> Result<Transaction, HarnessError> {
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        set_user_scope(&mut transaction, user_id).await?;
        Ok(transaction)
    }

    /// Install a component's final schema once; no historical data migration runs.
    pub async fn initialize(
        &self,
        component: &str,
        schema_version: i64,
        schema: &'static str,
        postgres_extras: &'static str,
    ) -> Result<(), HarnessError> {
        match sqlx::query_scalar::<_, i64>(
            "SELECT version FROM ternilo_schema WHERE component = $1",
        )
        .bind(component)
        .fetch_optional(&self.pool)
        .await
        {
            Ok(Some(version)) if version == schema_version => return Ok(()),
            Ok(Some(version)) => {
                return Err(HarnessError::invalid(format!(
                    "database schema for {component:?} is version {version}, but this build requires version {schema_version}"
                )));
            }
            Ok(None) => {}
            Err(error) if missing_schema_table(&error) => {}
            Err(error) => return Err(database_error(error)),
        }
        let mut transaction = self.begin().await?;
        lock(&mut transaction, "ternilo:schema").await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS ternilo_schema (component TEXT PRIMARY KEY, version BIGINT NOT NULL)")
            .execute(&mut *transaction).await.map_err(database_error)?;
        let version =
            sqlx::query_scalar::<_, i64>("SELECT version FROM ternilo_schema WHERE component = $1")
                .bind(component)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(database_error)?;
        match version {
            Some(version) if version == schema_version => {}
            Some(version) => {
                return Err(HarnessError::invalid(format!(
                    "database schema for {component:?} is version {version}, but this build requires version {schema_version}"
                )));
            }
            None => {
                sqlx::raw_sql(schema)
                    .execute(&mut *transaction)
                    .await
                    .map_err(database_error)?;
                if self.backend == Backend::Postgres && !postgres_extras.is_empty() {
                    sqlx::raw_sql(postgres_extras)
                        .execute(&mut *transaction)
                        .await
                        .map_err(database_error)?;
                }
                sqlx::query("INSERT INTO ternilo_schema (component, version) VALUES ($1, $2)")
                    .bind(component)
                    .bind(schema_version)
                    .execute(&mut *transaction)
                    .await
                    .map_err(database_error)?;
            }
        }
        transaction.commit().await.map_err(database_error)
    }

    pub async fn health(&self) -> Result<(), HarnessError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
        Ok(())
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

#[must_use]
pub fn backend(connection: &AnyConnection) -> Backend {
    match connection.backend_name() {
        "SQLite" => Backend::Sqlite,
        "PostgreSQL" => Backend::Postgres,
        _ => unreachable!("Database installs only SQLite and PostgreSQL connections"),
    }
}

pub async fn set_tenant_scope(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
) -> Result<(), HarnessError> {
    tenant_id.validate()?;
    if backend(transaction) == Backend::Postgres {
        sqlx::query("SELECT set_config('ternilo.tenant_id', $1, true)")
            .bind(tenant_id.as_str())
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
    }
    Ok(())
}

pub async fn set_user_scope(
    transaction: &mut Transaction,
    user_id: &UserId,
) -> Result<(), HarnessError> {
    user_id.validate()?;
    if backend(transaction) == Backend::Postgres {
        sqlx::query("SELECT set_config('ternilo.user_id', $1, true)")
            .bind(user_id.as_str())
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
    }
    Ok(())
}

fn missing_schema_table(error: &sqlx::Error) -> bool {
    error.as_database_error().is_some_and(|error| {
        error.code().as_deref() == Some("42P01")
            || (error.code().as_deref() == Some("1")
                && error.message() == "no such table: ternilo_schema")
    })
}

/// PostgreSQL serializes a logical resource; SQLite already owns the write lock.
pub async fn lock(transaction: &mut Transaction, key: &str) -> Result<(), HarnessError> {
    if backend(transaction) == Backend::Postgres {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(key)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
    }
    Ok(())
}

#[must_use]
pub fn for_update<'a>(connection: &AnyConnection, sqlite: &'a str, postgres: &'a str) -> &'a str {
    match backend(connection) {
        Backend::Sqlite => sqlite,
        Backend::Postgres => postgres,
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used directly as a map_err adapter."
)]
#[must_use]
pub fn database_error(error: sqlx::Error) -> HarnessError {
    HarnessError::execution(format!("server database error: {error}"))
}
