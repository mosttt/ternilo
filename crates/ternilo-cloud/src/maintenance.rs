use serde::Serialize;
use sqlx::Row;
use ternilo_control::{PlatformAction, authorize_platform_in};
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Backend, Database, Transaction, database_error};

use crate::CloudStore;

#[derive(Clone, Debug, Serialize)]
pub struct ExecutionMaintenance {
    pub claims_paused: bool,
    pub active_runs: u64,
    pub active_commands: u64,
}

pub(crate) async fn initialize_database(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "execution_maintenance",
            1,
            "",
            include_str!("schema/maintenance_postgres.sql"),
        )
        .await
}

impl CloudStore {
    pub async fn execution_maintenance(
        &self,
        actor: &UserId,
    ) -> Result<ExecutionMaintenance, HarnessError> {
        let mut transaction = self.begin().await?;
        authorize_platform_in(&mut transaction, actor, PlatformAction::WorkersRead).await?;
        let result = snapshot(&mut transaction, self.database.backend()).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(result)
    }

    pub async fn set_execution_paused(
        &self,
        actor: &UserId,
        paused: bool,
    ) -> Result<ExecutionMaintenance, HarnessError> {
        let mut transaction = self.begin().await?;
        authorize_platform_in(&mut transaction, actor, PlatformAction::WorkersManage).await?;
        let statement = if self.database.backend() == Backend::Postgres {
            "SELECT ternilo_pause_execution($1)"
        } else {
            "UPDATE cloud_runtime_control SET claims_paused=$1 WHERE singleton=1 RETURNING claims_paused"
        };
        sqlx::query_scalar::<_, i64>(statement)
            .bind(i64::from(paused))
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
        let result = snapshot(&mut transaction, self.database.backend()).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(result)
    }
}

async fn snapshot(
    transaction: &mut Transaction,
    backend: Backend,
) -> Result<ExecutionMaintenance, HarnessError> {
    let statement = if backend == Backend::Postgres {
        "SELECT * FROM ternilo_execution_maintenance()"
    } else {
        "SELECT runtime.claims_paused,
         (SELECT COUNT(*) FROM cloud_runs WHERE state IN ('leased','running','cancel_requested')) AS active_runs,
         (SELECT COUNT(*) FROM cloud_session_commands WHERE state='inflight') AS active_commands
         FROM cloud_runtime_control runtime WHERE singleton=1"
    };
    let row = sqlx::query(statement)
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
    Ok(ExecutionMaintenance {
        claims_paused: row
            .try_get::<i64, _>("claims_paused")
            .map_err(database_error)?
            != 0,
        active_runs: crate::store::from_i64(
            row.try_get("active_runs").map_err(database_error)?,
            "active run count",
        )?,
        active_commands: crate::store::from_i64(
            row.try_get("active_commands").map_err(database_error)?,
            "active command count",
        )?,
    })
}

#[cfg(test)]
mod tests {
    use ternilo_control::{
        ControlStore, InstanceMode, NativeRegistration, OidcPrincipal, PlatformRole, SecretCipher,
    };

    use super::*;

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep maintenance role changes and single-user authorization in one lifecycle test."
    )]
    async fn maintenance_uses_current_platform_role_and_instance_mode() {
        let directory = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}",
            directory.path().join("maintenance.sqlite3").display()
        );
        let control = ControlStore::connect(&url, None, SecretCipher::from_key([27; 32]), 2)
            .await
            .unwrap();
        let cloud = CloudStore::connect(&url, None, 2).await.unwrap();
        let owner = control
            .initialize_owner(
                &NativeRegistration {
                    email: "owner@example.test".to_owned(),
                    username: "owner".to_owned(),
                    password: "owner-password-123".to_owned(),
                },
                1_000,
            )
            .await
            .unwrap()
            .session
            .user;
        control
            .set_instance_mode(&owner, InstanceMode::MultiUser, 1, 1_001)
            .await
            .unwrap();
        let user = control
            .upsert_user(
                &OidcPrincipal {
                    issuer: "https://identity.test".to_owned(),
                    subject: "operator".to_owned(),
                    email: None,
                    display_name: None,
                },
                "test-operator",
                1_002,
            )
            .await
            .unwrap();
        assert!(cloud.execution_maintenance(&user.user_id).await.is_err());
        let operator = control
            .set_account_role(&owner, &user.user_id, PlatformRole::Operator, 1, 1_003)
            .await
            .unwrap();
        assert!(
            cloud
                .set_execution_paused(&user.user_id, true)
                .await
                .unwrap()
                .claims_paused
        );
        let auditor = control
            .set_account_role(
                &owner,
                &user.user_id,
                PlatformRole::Auditor,
                operator.role_revision,
                1_004,
            )
            .await
            .unwrap();
        assert!(
            cloud
                .execution_maintenance(&user.user_id)
                .await
                .unwrap()
                .claims_paused
        );
        assert!(
            cloud
                .set_execution_paused(&user.user_id, false)
                .await
                .is_err()
        );
        control
            .set_account_role(
                &owner,
                &user.user_id,
                PlatformRole::Operator,
                auditor.role_revision,
                1_005,
            )
            .await
            .unwrap();
        control
            .set_instance_mode(&owner, InstanceMode::SingleUser, 2, 1_006)
            .await
            .unwrap();
        assert!(cloud.execution_maintenance(&user.user_id).await.is_err());
        assert!(
            cloud
                .set_execution_paused(&user.user_id, false)
                .await
                .is_err()
        );
        assert!(
            !cloud
                .set_execution_paused(&owner.user_id, false)
                .await
                .unwrap()
                .claims_paused
        );
    }
}
