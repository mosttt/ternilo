use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId};
use ternilo_storage::{Database, Json, Transaction, database_error};
use ternilo_transport::{ExecutorHello, ExecutorId};

use crate::{
    ControlAction, ControlStore, ControlUser,
    store::{append_audit, from_i64, require_action, to_i64},
};

pub(crate) mod enrollment;
pub(crate) mod names;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComputerManagement {
    pub name: String,
    pub notes: String,
    pub suspended_at_ms: Option<u64>,
    pub removed_at_ms: Option<u64>,
    pub revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUpdate {
    pub name: String,
    pub notes: String,
    pub expected_revision: u64,
}

#[derive(Serialize)]
pub struct ComputerDetails {
    pub executor: crate::ExecutorRecord,
    pub management: ComputerManagement,
    pub owner: ControlUser,
    pub hello: Option<ExecutorHello>,
    pub workspace_count: u64,
    pub session_count: u64,
    pub credential_issued_at_ms: Option<u64>,
    pub credential_last_used_at_ms: Option<u64>,
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "computer_management",
            1,
            include_str!("schema.sql"),
            include_str!("postgres.sql"),
        )
        .await?;
    names::initialize(database).await
}

pub(crate) async fn management_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
) -> Result<ComputerManagement, HarnessError> {
    let row = sqlx::query(
        "SELECT COALESCE(display_name,$3) AS name,notes,suspended_at_ms,removed_at_ms,revision
        FROM control_computer_management WHERE tenant_id=$1 AND executor_id=$2",
    )
    .bind(tenant.as_str())
    .bind(executor.as_str())
    .bind(executor.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    let Some(row) = row else {
        return Ok(ComputerManagement {
            name: executor.to_string(),
            ..ComputerManagement::default()
        });
    };
    management_record(&row)
}

pub(crate) async fn persist_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
    management: &ComputerManagement,
) -> Result<(), HarnessError> {
    sqlx::query("INSERT INTO control_computer_management
        (tenant_id,executor_id,display_name,notes,suspended_at_ms,removed_at_ms,revision)
        VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant_id,executor_id) DO UPDATE SET
        display_name=EXCLUDED.display_name,notes=EXCLUDED.notes,suspended_at_ms=EXCLUDED.suspended_at_ms,
        removed_at_ms=EXCLUDED.removed_at_ms,revision=EXCLUDED.revision")
        .bind(tenant.as_str()).bind(executor.as_str()).bind(&management.name).bind(&management.notes)
        .bind(management.suspended_at_ms.map(|value| to_i64(value,"suspension timestamp")).transpose()?)
        .bind(management.removed_at_ms.map(|value| to_i64(value,"removal timestamp")).transpose()?)
        .bind(to_i64(management.revision,"computer revision")?)
        .execute(&mut **tx).await.map_err(database_error)?;
    Ok(())
}

async fn require_computer_in(
    tx: &mut Transaction,
    actor: &ControlUser,
    tenant: &TenantId,
    executor: &ExecutorId,
    owned_only: bool,
    write: bool,
) -> Result<(String, Option<String>, String), HarnessError> {
    tenant.validate()?;
    executor.validate()?;
    require_action(
        tx,
        tenant,
        &actor.user_id,
        if owned_only {
            if write {
                ControlAction::RunReserve
            } else {
                ControlAction::ExecutorRead
            }
        } else {
            ControlAction::ExecutorManage
        },
    )
    .await?;
    if write {
        ternilo_storage::lock(tx, &format!("computer-management:{tenant}:{executor}")).await?;
    }
    let sql = if write {
        ternilo_storage::for_update(
            tx,
            "SELECT owner_user_id,project_id,state FROM control_executors WHERE tenant_id=$1 AND executor_id=$2 AND (NOT $3 OR owner_user_id=$4)",
            "SELECT owner_user_id,project_id,state FROM control_executors WHERE tenant_id=$1 AND executor_id=$2 AND (NOT $3 OR owner_user_id=$4) FOR UPDATE",
        )
    } else {
        "SELECT owner_user_id,project_id,state FROM control_executors WHERE tenant_id=$1 AND executor_id=$2 AND (NOT $3 OR owner_user_id=$4)"
    };
    let row = sqlx::query(sql)
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .bind(owned_only)
        .bind(actor.user_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("computer does not exist in the permitted scope"))?;
    Ok((
        row.try_get("owner_user_id").map_err(database_error)?,
        row.try_get("project_id").map_err(database_error)?,
        row.try_get("state").map_err(database_error)?,
    ))
}

pub(crate) fn management_record(
    row: &sqlx::any::AnyRow,
) -> Result<ComputerManagement, HarnessError> {
    let timestamp = |field| {
        row.try_get::<Option<i64>, _>(field)
            .map_err(database_error)?
            .map(|value| from_i64(value, field))
            .transpose()
    };
    Ok(ComputerManagement {
        name: row.try_get("name").map_err(database_error)?,
        notes: row.try_get("notes").map_err(database_error)?,
        suspended_at_ms: timestamp("suspended_at_ms")?,
        removed_at_ms: timestamp("removed_at_ms")?,
        revision: from_i64(
            row.try_get("revision").map_err(database_error)?,
            "computer revision",
        )?,
    })
}

fn advance(management: &mut ComputerManagement, expected: u64) -> Result<(), HarnessError> {
    if management.removed_at_ms.is_some() {
        return Err(HarnessError::conflict("computer registration was removed"));
    }
    if management.revision != expected {
        return Err(HarnessError::conflict(
            "computer settings changed; refresh before retrying",
        ));
    }
    management.revision = management
        .revision
        .checked_add(1)
        .ok_or_else(|| HarnessError::conflict("computer revision exhausted"))?;
    Ok(())
}

impl ControlStore {
    pub async fn computer_display_names(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executors: &[ExecutorId],
    ) -> Result<std::collections::BTreeMap<ExecutorId, String>, HarnessError> {
        if executors.is_empty() {
            return Ok(std::collections::BTreeMap::new());
        }
        let mut tx = self.database.tenant_transaction(tenant).await?;
        require_action(&mut tx, tenant, &actor.user_id, ControlAction::TenantRead).await?;
        let placeholders = (2..=executors.len() + 1)
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT e.executor_id,COALESCE(m.display_name,e.executor_id) AS name FROM control_executors e LEFT JOIN control_computer_management m ON m.tenant_id=e.tenant_id AND m.executor_id=e.executor_id WHERE e.tenant_id=$1 AND e.executor_id IN ({placeholders})"
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(tenant.as_str());
        for executor in executors {
            query = query.bind(executor.as_str());
        }
        let rows = query.fetch_all(&mut *tx).await.map_err(database_error)?;
        let result = rows
            .iter()
            .map(|row| {
                Ok((
                    ExecutorId::new(
                        row.try_get::<String, _>("executor_id")
                            .map_err(database_error)?,
                    ),
                    row.try_get::<String, _>("name").map_err(database_error)?,
                ))
            })
            .collect();
        tx.commit().await.map_err(database_error)?;
        result
    }

    pub async fn computer_details(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        owned_only: bool,
    ) -> Result<ComputerDetails, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        let (owner, _, _) =
            require_computer_in(&mut tx, actor, tenant, executor, owned_only, false).await?;
        let row = sqlx::query(concat!(
            include_str!("executor_select.sql"),
            " WHERE e.tenant_id=$1 AND e.executor_id=$2"
        ))
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        let record = crate::store::executor_record(&row)?;
        let username = sqlx::query_scalar("SELECT username FROM control_users WHERE user_id=$1")
            .bind(&owner)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        let hello = sqlx::query_scalar::<_, Json<ExecutorHello>>(
            "SELECT hello_json FROM control_edge_executors WHERE tenant_id=$1 AND executor_id=$2",
        )
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?
        .map(|value| value.0);
        let workspace_count:i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_workspaces WHERE tenant_id=$1 AND executor_id=$2 AND unregistered_at_ms IS NULL")
            .bind(tenant.as_str()).bind(executor.as_str()).fetch_one(&mut *tx).await.map_err(database_error)?;
        let session_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM control_edge_sessions WHERE tenant_id=$1 AND executor_id=$2",
        )
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        let credential = sqlx::query("SELECT issued_at_ms,last_used_at_ms FROM control_node_credentials WHERE tenant_id=$1 AND executor_id=$2 ORDER BY (revoked_at_ms IS NULL) DESC,issued_at_ms DESC LIMIT 1")
            .bind(tenant.as_str()).bind(executor.as_str()).fetch_optional(&mut *tx).await.map_err(database_error)?;
        let issued = credential
            .as_ref()
            .map(|row| {
                row.try_get::<i64, _>("issued_at_ms")
                    .map_err(database_error)
                    .and_then(|value| from_i64(value, "credential issue timestamp"))
            })
            .transpose()?;
        let used = credential
            .as_ref()
            .map(|row| {
                row.try_get::<Option<i64>, _>("last_used_at_ms")
                    .map_err(database_error)?
                    .map(|value| from_i64(value, "credential last-used timestamp"))
                    .transpose()
            })
            .transpose()?
            .flatten();
        tx.commit().await.map_err(database_error)?;
        Ok(ComputerDetails {
            management: record.management.clone(),
            executor: record,
            owner: ControlUser {
                user_id: ternilo_protocol::UserId::new(owner),
                username,
            },
            hello,
            workspace_count: from_i64(workspace_count, "workspace count")?,
            session_count: from_i64(session_count, "session count")?,
            credential_issued_at_ms: issued,
            credential_last_used_at_ms: used,
        })
    }
    pub async fn computer_management(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        owned_only: bool,
    ) -> Result<ComputerManagement, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        require_computer_in(&mut tx, actor, tenant, executor, owned_only, false).await?;
        let result = management_in(&mut tx, tenant, executor).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }

    pub async fn update_computer(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        owned_only: bool,
        input: &ComputerUpdate,
        now_ms: u64,
    ) -> Result<ComputerManagement, HarnessError> {
        let name = names::validate(&input.name)?;
        if input.notes.chars().count() > 4000 {
            return Err(HarnessError::invalid(
                "computer notes exceed 4000 characters",
            ));
        }
        let mut tx = self.database.tenant_transaction(tenant).await?;
        let (owner, _, _) =
            require_computer_in(&mut tx, actor, tenant, executor, owned_only, true).await?;
        let mut management = management_in(&mut tx, tenant, executor).await?;
        advance(&mut management, input.expected_revision)?;
        names::reserve_in(&mut tx, tenant, executor, &owner, name, None, now_ms).await?;
        management.name = name.to_owned();
        management.notes.clone_from(&input.notes);
        persist_in(&mut tx, tenant, executor, &management).await?;
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            "executor.update",
            "executor",
            executor.as_str(),
            "success",
            json!({"revision":management.revision}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(management)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Keep actor scope and the expected management version explicit."
    )]
    pub async fn set_computer_suspended(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        owned_only: bool,
        suspended: bool,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<ComputerManagement, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        let (_, _, state) =
            require_computer_in(&mut tx, actor, tenant, executor, owned_only, true).await?;
        if state == "revoked" {
            return Err(HarnessError::conflict(
                "a revoked computer must be enrolled again",
            ));
        }
        let mut management = management_in(&mut tx, tenant, executor).await?;
        advance(&mut management, expected_revision)?;
        management.suspended_at_ms = suspended.then_some(now_ms);
        persist_in(&mut tx, tenant, executor, &management).await?;
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            if suspended {
                "executor.suspend"
            } else {
                "executor.resume"
            },
            "executor",
            executor.as_str(),
            "success",
            json!({"revision":management.revision}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(management)
    }

    pub async fn remove_computer_registration(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        owned_only: bool,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        require_action(
            &mut tx,
            tenant,
            &actor.user_id,
            if owned_only {
                ControlAction::RunReserve
            } else {
                ControlAction::ExecutorManage
            },
        )
        .await?;
        ternilo_storage::lock(&mut tx, &format!("computer-management:{tenant}:{executor}")).await?;
        // Follow enrollment/revocation lock order: grants, executor, credential.
        sqlx::query("UPDATE control_executor_enrollments SET consumed_at_ms=$3 WHERE tenant_id=$1 AND executor_id=$2 AND consumed_at_ms IS NULL AND EXISTS (SELECT 1 FROM control_executors WHERE tenant_id=$1 AND executor_id=$2 AND (NOT $4 OR owner_user_id=$5))")
            .bind(tenant.as_str()).bind(executor.as_str()).bind(to_i64(now_ms,"removal timestamp")?).bind(owned_only).bind(actor.user_id.as_str()).execute(&mut *tx).await.map_err(database_error)?;
        require_computer_in(&mut tx, actor, tenant, executor, owned_only, true).await?;
        let mut management = management_in(&mut tx, tenant, executor).await?;
        advance(&mut management, expected_revision)?;
        management.removed_at_ms = Some(now_ms);
        management.suspended_at_ms = None;
        sqlx::query(
            "UPDATE control_executors SET state='revoked' WHERE tenant_id=$1 AND executor_id=$2",
        )
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        sqlx::query("UPDATE control_node_credentials SET revoked_at_ms=COALESCE(revoked_at_ms,$3) WHERE tenant_id=$1 AND executor_id=$2")
            .bind(tenant.as_str()).bind(executor.as_str()).bind(to_i64(now_ms,"removal timestamp")?).execute(&mut *tx).await.map_err(database_error)?;
        persist_in(&mut tx, tenant, executor, &management).await?;
        sqlx::query("UPDATE control_computer_names SET name_key=NULL,reserved_until_ms=NULL WHERE tenant_id=$1 AND executor_id=$2")
            .bind(tenant.as_str()).bind(executor.as_str()).execute(&mut *tx).await.map_err(database_error)?;
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            "executor.registration.remove",
            "executor",
            executor.as_str(),
            "success",
            json!({"revision":management.revision,"history_retained":true}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }
}
