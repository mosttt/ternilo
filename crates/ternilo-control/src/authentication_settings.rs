use sqlx::Row;
use ternilo_protocol::HarnessError;
use ternilo_storage::{Backend, Database, Transaction, database_error, lock};
use zeroize::Zeroizing;

use crate::{
    ControlStore, ControlUser, EncryptedSecret, PlatformRole, SecretCipher,
    account_store::{append_platform_audit, platform_role_in},
};

const SCOPE: &str = "server-authentication-v1";
const SQLITE_SCHEMA: &str = "CREATE TABLE control_authentication_settings (
    singleton BIGINT PRIMARY KEY CHECK (singleton = 1),
    revision BIGINT NOT NULL CHECK (revision > 0),
    nonce BLOB NOT NULL,
    ciphertext BLOB NOT NULL
);";
const POSTGRES_SCHEMA: &str = "CREATE TABLE control_authentication_settings (
    singleton BIGINT PRIMARY KEY CHECK (singleton = 1),
    revision BIGINT NOT NULL CHECK (revision > 0),
    nonce BYTEA NOT NULL,
    ciphertext BYTEA NOT NULL
);";
const POSTGRES_ACCESS: &str = "
REVOKE ALL ON control_authentication_settings FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_authentication_settings TO ternilo_runtime;
END IF;
END $$;";

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "authentication",
            1,
            match database.backend() {
                Backend::Sqlite => SQLITE_SCHEMA,
                Backend::Postgres => POSTGRES_SCHEMA,
            },
            POSTGRES_ACCESS,
        )
        .await
}

impl ControlStore {
    pub async fn reset_authentication_settings(&self, now_ms: u64) -> Result<(), HarnessError> {
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        let instance = crate::identity_store::required_instance(&mut transaction).await?;
        sqlx::query("DELETE FROM control_authentication_settings WHERE singleton = 1")
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        append_platform_audit(
            &mut transaction,
            &instance.owner_user_id,
            "instance.authentication.reset",
            "default",
            serde_json::json!({"source": "operator_cli"}),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn require_authentication_settings_owner(
        &self,
        actor: &ControlUser,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.database.begin().await?;
        require_owner(&mut transaction, actor).await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn authentication_settings_revision(&self) -> Result<u64, HarnessError> {
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM control_authentication_settings WHERE singleton = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(revision.unwrap_or(0).cast_unsigned())
    }

    pub async fn authentication_settings(
        &self,
    ) -> Result<Option<(u64, Zeroizing<Vec<u8>>)>, HarnessError> {
        let row = sqlx::query("SELECT revision, nonce, ciphertext FROM control_authentication_settings WHERE singleton = 1")
            .fetch_optional(&self.pool).await.map_err(database_error)?;
        row.map(|row| {
            let revision = row
                .try_get::<i64, _>("revision")
                .map_err(database_error)?
                .cast_unsigned();
            let secret = encrypted(&row)?;
            Ok((
                revision,
                self.cipher
                    .decrypt(SCOPE, None, "settings", revision, &secret)?,
            ))
        })
        .transpose()
    }

    pub async fn set_authentication_settings(
        &self,
        actor: &ControlUser,
        revision: u64,
        value: &[u8],
        now_ms: u64,
    ) -> Result<u64, HarnessError> {
        if value.len() > 32 * 1024 {
            return Err(HarnessError::invalid(
                "authentication settings exceed 32 KiB",
            ));
        }
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        require_owner(&mut transaction, actor).await?;
        let previous: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM control_authentication_settings WHERE singleton = 1",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if previous.unwrap_or(0).cast_unsigned() != revision {
            return Err(HarnessError::conflict(
                "authentication settings changed; reload before saving",
            ));
        }
        let next_revision = i64::try_from(revision)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| HarnessError::invalid("authentication settings revision overflow"))?;
        let secret = self.cipher.encrypt(
            SCOPE,
            None,
            "settings",
            next_revision.cast_unsigned(),
            value,
        )?;
        sqlx::query("INSERT INTO control_authentication_settings (singleton, revision, nonce, ciphertext) VALUES (1, $1, $2, $3) ON CONFLICT (singleton) DO UPDATE SET revision = EXCLUDED.revision, nonce = EXCLUDED.nonce, ciphertext = EXCLUDED.ciphertext")
            .bind(next_revision).bind(secret.nonce.to_vec()).bind(secret.ciphertext)
            .execute(&mut *transaction).await.map_err(database_error)?;
        append_platform_audit(
            &mut transaction,
            &actor.user_id,
            "instance.authentication",
            "default",
            serde_json::json!({"revision": next_revision}),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(next_revision.cast_unsigned())
    }
}

async fn require_owner(
    transaction: &mut Transaction,
    actor: &ControlUser,
) -> Result<(), HarnessError> {
    if platform_role_in(transaction, &actor.user_id).await? != PlatformRole::Owner {
        return Err(HarnessError::policy(
            "only the instance owner can configure authentication",
        ));
    }
    Ok(())
}

fn encrypted(row: &sqlx::any::AnyRow) -> Result<EncryptedSecret, HarnessError> {
    Ok(EncryptedSecret {
        nonce: row
            .try_get::<Vec<u8>, _>("nonce")
            .map_err(database_error)?
            .try_into()
            .map_err(|_| HarnessError::execution("invalid authentication settings nonce"))?,
        ciphertext: row.try_get("ciphertext").map_err(database_error)?,
    })
}

pub(crate) async fn rotate(
    transaction: &mut Transaction,
    current: &SecretCipher,
    next: &SecretCipher,
) -> Result<u64, HarnessError> {
    let row = sqlx::query("SELECT revision, nonce, ciphertext FROM control_authentication_settings WHERE singleton = 1")
        .fetch_optional(&mut **transaction).await.map_err(database_error)?;
    let Some(row) = row else { return Ok(0) };
    let revision = row
        .try_get::<i64, _>("revision")
        .map_err(database_error)?
        .cast_unsigned();
    let plaintext = current.decrypt(SCOPE, None, "settings", revision, &encrypted(&row)?)?;
    let replacement = next.encrypt(SCOPE, None, "settings", revision, &plaintext)?;
    sqlx::query("UPDATE control_authentication_settings SET nonce = $1, ciphertext = $2 WHERE singleton = 1")
        .bind(replacement.nonce.to_vec()).bind(replacement.ciphertext)
        .execute(&mut **transaction).await.map_err(database_error)?;
    Ok(1)
}
