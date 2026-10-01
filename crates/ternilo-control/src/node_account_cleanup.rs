use crate::{ControlStore, ControlUser, EdgeStore, NodePrincipal};
use sqlx::Row;
use ternilo_protocol::{HarnessError, InputAuthor, InputProvenance, TenantId, UserId};
use ternilo_storage::{Backend, Database, Transaction, database_error, set_tenant_scope};
use ternilo_transport::{
    ExecutorId, NodeAccountAuthorization, NodeCleanupRequest, NodeCleanupSnapshot,
    NodeCleanupState, NodeInputAuthorization,
};

#[cfg(test)]
mod tests;

/// A credential instance authenticated exclusively for its cleanup channel.
#[derive(Clone, Debug)]
struct CleanupPrincipal {
    tenant: TenantId,
    executor: ExecutorId,
    credential: String,
    owner: UserId,
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "node_account_cleanup",
            1,
            include_str!("node_account_cleanup/schema.sql"),
            include_str!("node_account_cleanup/postgres.sql"),
        )
        .await
}

pub(crate) async fn stage_in(
    tx: &mut Transaction,
    actor: &ControlUser,
    user_id: &UserId,
    revision: u64,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let rows = sqlx::query(match ternilo_storage::backend(tx) {
        Backend::Postgres => {
            "SELECT tenant_id,executor_id,credential_id FROM ternilo_node_cleanup_targets($1,$2)"
        }
        Backend::Sqlite => {
            "SELECT DISTINCT c.tenant_id,c.executor_id,c.credential_id
            FROM control_node_credentials c JOIN control_executors e
            ON e.tenant_id=c.tenant_id AND e.executor_id=c.executor_id
            WHERE $1<>$2
              AND (e.owner_user_id=$2 OR EXISTS (SELECT 1 FROM control_node_input_authorizations a
                WHERE a.credential_id=c.credential_id AND a.user_id=$2))
            ORDER BY c.tenant_id,c.executor_id,c.credential_id"
        }
    })
    .bind(actor.user_id.as_str())
    .bind(user_id.as_str())
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    for row in rows {
        let tenant = TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        );
        set_tenant_scope(tx, &tenant).await?;
        sqlx::query("INSERT INTO control_node_account_cleanup
            (tenant_id,executor_id,credential_id,request_id,user_id,status_revision,created_at_ms)
            VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (credential_id,user_id,status_revision) DO NOTHING")
            .bind(tenant.as_str()).bind(row.try_get::<String,_>("executor_id").map_err(database_error)?)
            .bind(row.try_get::<String,_>("credential_id").map_err(database_error)?)
            .bind(crate::crypto::random_identifier("ter_nc")).bind(user_id.as_str())
            .bind(number(revision)?).bind(number(now_ms)?)
            .execute(&mut **tx).await.map_err(database_error)?;
    }
    Ok(())
}

impl EdgeStore {
    pub async fn node_input_authorization(
        &self,
        node: &NodePrincipal,
        user_id: &UserId,
    ) -> Result<NodeInputAuthorization, HarnessError> {
        self.require_node_credential(node).await?;
        let mut tx = self
            .database()
            .tenant_transaction(&node.scope.tenant_id)
            .await?;
        let row = sqlx::query(
            "SELECT status_revision FROM control_users WHERE user_id=$1 AND status='active'",
        )
        .bind(user_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("input account is no longer active"))?;
        let revision = unsigned(row.try_get("status_revision").map_err(database_error)?)?;
        tx.commit().await.map_err(database_error)?;
        Ok(NodeInputAuthorization {
            credential_id: node.credential_id.clone(),
            status_revision: revision,
        })
    }

    pub async fn record_node_input_authorization_in(
        tx: &mut Transaction,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        provenance: &InputProvenance,
        authorization: &NodeInputAuthorization,
    ) -> Result<(), HarnessError> {
        let InputAuthor::Account { user_id, .. } = &provenance.author else {
            return Err(HarnessError::policy(
                "Node authorization requires an account author",
            ));
        };
        ternilo_storage::lock(tx, &format!("ternilo:account-role:{user_id}")).await?;
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT status_revision FROM control_users WHERE user_id=$1 AND status='active'",
        )
        .bind(user_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
        if revision != Some(number(authorization.status_revision)?) {
            return Err(HarnessError::policy(
                "Node input account authorization is no longer current",
            ));
        }
        let valid: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM control_node_credentials c
            JOIN control_executors e ON e.tenant_id=c.tenant_id AND e.executor_id=c.executor_id
            WHERE c.tenant_id=$1 AND c.executor_id=$2 AND c.credential_id=$3
              AND c.revoked_at_ms IS NULL AND e.state<>'revoked'",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(&authorization.credential_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        if valid != 1 {
            return Err(HarnessError::policy(
                "input Node credential is no longer active",
            ));
        }
        sqlx::query(
            "INSERT INTO control_node_input_authorizations
            (tenant_id,executor_id,input_id,user_id,status_revision,credential_id)
            VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (tenant_id,executor_id,input_id) DO NOTHING",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(provenance.input_id.as_str())
        .bind(user_id.as_str())
        .bind(number(authorization.status_revision)?)
        .bind(&authorization.credential_id)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
        let row = sqlx::query(
            "SELECT user_id,status_revision,credential_id FROM control_node_input_authorizations
            WHERE tenant_id=$1 AND executor_id=$2 AND input_id=$3",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(provenance.input_id.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        if row
            .try_get::<String, _>("user_id")
            .map_err(database_error)?
            != user_id.as_str()
            || unsigned(row.try_get("status_revision").map_err(database_error)?)?
                != authorization.status_revision
            || row
                .try_get::<String, _>("credential_id")
                .map_err(database_error)?
                != authorization.credential_id
        {
            return Err(HarnessError::conflict(
                "accepted Node input authorization cannot change",
            ));
        }
        Ok(())
    }
}

impl ControlStore {
    pub async fn node_cleanup_snapshot(
        &self,
        token: &str,
    ) -> Result<NodeCleanupSnapshot, HarnessError> {
        if token.is_empty() || token.len() > 256 {
            return Err(HarnessError::policy("invalid Node cleanup credential"));
        }
        let mut tx = self.database.begin().await?;
        let hash = crate::crypto::token_hash(token).to_vec();
        let tenant = crate::store::token_tenant(&mut tx, &hash, false).await?;
        set_tenant_scope(&mut tx, &tenant).await?;
        let row = sqlx::query("SELECT c.credential_id,c.executor_id,c.revoked_at_ms,e.owner_user_id,e.state,u.status,m.suspended_at_ms,m.removed_at_ms
            FROM control_node_credentials c JOIN control_executors e
            ON e.tenant_id=c.tenant_id AND e.executor_id=c.executor_id
            JOIN control_users u ON u.user_id=e.owner_user_id LEFT JOIN control_computer_management m ON m.tenant_id=e.tenant_id AND m.executor_id=e.executor_id WHERE c.tenant_id=$1 AND c.token_hash=$2")
            .bind(tenant.as_str()).bind(hash).fetch_optional(&mut *tx).await.map_err(database_error)?
            .ok_or_else(|| HarnessError::policy("invalid Node cleanup credential"))?;
        let principal = CleanupPrincipal {
            tenant,
            executor: ExecutorId::new(
                row.try_get::<String, _>("executor_id")
                    .map_err(database_error)?,
            ),
            credential: row.try_get("credential_id").map_err(database_error)?,
            owner: UserId::new(
                row.try_get::<String, _>("owner_user_id")
                    .map_err(database_error)?,
            ),
        };
        let requests = cleanup_requests_in(&mut tx, &principal).await?;
        let active = row
            .try_get::<Option<i64>, _>("revoked_at_ms")
            .map_err(database_error)?
            .is_none()
            && row.try_get::<String, _>("state").map_err(database_error)? != "revoked"
            && row.try_get::<String, _>("status").map_err(database_error)? == "active";
        if !active && requests.is_empty() {
            return Err(HarnessError::policy(
                "revoked credential has no pending Node cleanup",
            ));
        }
        let rows = sqlx::query(
            "SELECT user_id,status,status_revision FROM control_users
            WHERE user_id=$1 OR user_id IN (SELECT user_id FROM control_node_input_authorizations
                WHERE tenant_id=$2 AND executor_id=$3) ORDER BY user_id",
        )
        .bind(principal.owner.as_str())
        .bind(principal.tenant.as_str())
        .bind(principal.executor.as_str())
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
        let mut authorizations = Vec::with_capacity(rows.len());
        for row in rows {
            authorizations.push(NodeAccountAuthorization {
                user_id: UserId::new(
                    row.try_get::<String, _>("user_id")
                        .map_err(database_error)?,
                ),
                status_revision: unsigned(row.try_get("status_revision").map_err(database_error)?)?,
                active: row.try_get::<String, _>("status").map_err(database_error)? == "active",
            });
        }
        let server_id: String = sqlx::query_scalar(
            "SELECT owner_user_id FROM control_instance_settings WHERE singleton=1",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        let snapshot = NodeCleanupSnapshot {
            protocol_version: 1,
            server_id,
            tenant_id: principal.tenant,
            executor_id: principal.executor,
            credential_id: principal.credential,
            connection_allowed: active
                && row
                    .try_get::<Option<i64>, _>("suspended_at_ms")
                    .map_err(database_error)?
                    .is_none()
                && row
                    .try_get::<Option<i64>, _>("removed_at_ms")
                    .map_err(database_error)?
                    .is_none(),
            authorizations,
            requests,
        };
        tx.commit().await.map_err(database_error)?;
        Ok(snapshot)
    }
}

async fn cleanup_requests_in(
    tx: &mut Transaction,
    node: &CleanupPrincipal,
) -> Result<Vec<NodeCleanupRequest>, HarnessError> {
    let rows = sqlx::query("SELECT request_id,user_id,status_revision,created_at_ms,state,detail,confirmed_at_ms FROM control_node_account_cleanup
        WHERE tenant_id=$1 AND executor_id=$2 AND credential_id=$3 ORDER BY created_at_ms,request_id")
        .bind(node.tenant.as_str()).bind(node.executor.as_str()).bind(&node.credential)
        .fetch_all(&mut **tx).await.map_err(database_error)?;
    rows.into_iter()
        .map(|row| {
            Ok(NodeCleanupRequest {
                request_id: row.try_get("request_id").map_err(database_error)?,
                user_id: UserId::new(
                    row.try_get::<String, _>("user_id")
                        .map_err(database_error)?,
                ),
                status_revision: unsigned(row.try_get("status_revision").map_err(database_error)?)?,
                created_at_ms: unsigned(row.try_get("created_at_ms").map_err(database_error)?)?,
                state: if row.try_get::<String, _>("state").map_err(database_error)? == "confirmed"
                {
                    NodeCleanupState::Confirmed
                } else {
                    NodeCleanupState::Pending
                },
                detail: row.try_get("detail").map_err(database_error)?,
                confirmed_at_ms: row
                    .try_get::<Option<i64>, _>("confirmed_at_ms")
                    .map_err(database_error)?
                    .map(unsigned)
                    .transpose()?,
            })
        })
        .collect()
}
fn number(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value)
        .map_err(|_| HarnessError::invalid("Node cleanup value exceeds database range"))
}
fn unsigned(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution("negative Node cleanup value"))
}

impl ControlStore {
    pub async fn record_node_cleanup_receipt(
        &self,
        token: &str,
        receipt: &ternilo_transport::NodeCleanupReceipt,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        if token.is_empty()
            || token.len() > 256
            || receipt.request_id.len() > 128
            || !matches!(
                receipt.detail.as_deref(),
                None | Some(
                    "process_state_unknown"
                        | "process_exit_pending"
                        | "session_busy"
                        | "cleanup_failed"
                )
            )
        {
            return Err(HarnessError::invalid("invalid Node cleanup receipt"));
        }
        let hash = crate::crypto::token_hash(token).to_vec();
        let mut tx = self.database.begin().await?;
        let tenant = crate::store::token_tenant(&mut tx, &hash, false).await?;
        set_tenant_scope(&mut tx, &tenant).await?;
        let credential: String=sqlx::query_scalar("SELECT credential_id FROM control_node_credentials WHERE tenant_id=$1 AND token_hash=$2")
            .bind(tenant.as_str()).bind(hash).fetch_one(&mut *tx).await.map_err(database_error)?;
        let storage:Option<String>=sqlx::query_scalar("SELECT storage_instance_id FROM control_node_storage_bindings WHERE tenant_id=$1 AND credential_id=$2")
            .bind(tenant.as_str()).bind(&credential).fetch_optional(&mut *tx).await.map_err(database_error)?;
        if storage.as_deref() != Some(receipt.storage_instance_id.as_str()) {
            return Err(HarnessError::policy(
                "cleanup receipt belongs to another Node storage instance",
            ));
        }
        let row=sqlx::query("SELECT state,status_revision FROM control_node_account_cleanup WHERE tenant_id=$1 AND credential_id=$2 AND request_id=$3")
            .bind(tenant.as_str()).bind(&credential).bind(&receipt.request_id).fetch_optional(&mut *tx).await.map_err(database_error)?
            .ok_or_else(||HarnessError::policy("cleanup receipt belongs to another credential instance"))?;
        if unsigned(row.try_get("status_revision").map_err(database_error)?)?
            != receipt.status_revision
        {
            return Err(HarnessError::policy(
                "cleanup receipt has another account revocation version",
            ));
        }
        if row.try_get::<String, _>("state").map_err(database_error)? != "confirmed" {
            let confirmed = receipt.state == NodeCleanupState::Confirmed;
            sqlx::query(
                "UPDATE control_node_account_cleanup SET state=$4,detail=$5,confirmed_at_ms=$6
                WHERE tenant_id=$1 AND credential_id=$2 AND request_id=$3 AND state='pending'",
            )
            .bind(tenant.as_str())
            .bind(&credential)
            .bind(&receipt.request_id)
            .bind(if confirmed { "confirmed" } else { "pending" })
            .bind(if confirmed {
                None
            } else {
                receipt.detail.as_deref()
            })
            .bind(if confirmed {
                Some(number(now_ms)?)
            } else {
                None
            })
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        }
        tx.commit().await.map_err(database_error)
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AccountNodeCleanup {
    pub tenant_id: TenantId,
    pub executor_id: ExecutorId,
    pub request: NodeCleanupRequest,
}

impl ControlStore {
    pub async fn account_node_cleanup(
        &self,
        actor: &ControlUser,
        user_id: &UserId,
    ) -> Result<Vec<AccountNodeCleanup>, HarnessError> {
        user_id.validate()?;
        let mut tx = self.database.begin().await?;
        crate::authorize_platform_in(&mut tx, &actor.user_id, crate::PlatformAction::AccountsRead)
            .await?;
        let rows=sqlx::query(match ternilo_storage::backend(&tx) {
            Backend::Postgres=>"SELECT tenant_id,executor_id,request_id,user_id,status_revision,created_at_ms,state,detail,confirmed_at_ms FROM ternilo_account_cleanup_records($1,$2)",
            Backend::Sqlite=>"SELECT tenant_id,executor_id,request_id,user_id,status_revision,created_at_ms,state,detail,confirmed_at_ms FROM control_node_account_cleanup WHERE user_id=$2 AND $1<>'' ORDER BY created_at_ms DESC,request_id",
        }).bind(actor.user_id.as_str()).bind(user_id.as_str()).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            records.push(AccountNodeCleanup {
                tenant_id: TenantId::new(
                    row.try_get::<String, _>("tenant_id")
                        .map_err(database_error)?,
                ),
                executor_id: ExecutorId::new(
                    row.try_get::<String, _>("executor_id")
                        .map_err(database_error)?,
                ),
                request: NodeCleanupRequest {
                    request_id: row.try_get("request_id").map_err(database_error)?,
                    user_id: UserId::new(
                        row.try_get::<String, _>("user_id")
                            .map_err(database_error)?,
                    ),
                    status_revision: unsigned(
                        row.try_get("status_revision").map_err(database_error)?,
                    )?,
                    created_at_ms: unsigned(row.try_get("created_at_ms").map_err(database_error)?)?,
                    state: if row.try_get::<String, _>("state").map_err(database_error)?
                        == "confirmed"
                    {
                        NodeCleanupState::Confirmed
                    } else {
                        NodeCleanupState::Pending
                    },
                    detail: row.try_get("detail").map_err(database_error)?,
                    confirmed_at_ms: row
                        .try_get::<Option<i64>, _>("confirmed_at_ms")
                        .map_err(database_error)?
                        .map(unsigned)
                        .transpose()?,
                },
            });
        }
        tx.commit().await.map_err(database_error)?;
        Ok(records)
    }
}

impl ControlStore {
    pub async fn synchronize_node_cleanup(
        &self,
        token: &str,
        storage_instance_id: &str,
        now_ms: u64,
    ) -> Result<NodeCleanupSnapshot, HarnessError> {
        if storage_instance_id.is_empty()
            || storage_instance_id.len() > 128
            || storage_instance_id.chars().any(char::is_whitespace)
        {
            return Err(HarnessError::invalid(
                "invalid Node storage instance identity",
            ));
        }
        let snapshot = self.node_cleanup_snapshot(token).await?;
        let mut tx = self
            .database
            .tenant_transaction(&snapshot.tenant_id)
            .await?;
        let last_used:Option<i64>=sqlx::query_scalar(ternilo_storage::for_update(&tx,
            "SELECT last_used_at_ms FROM control_node_credentials WHERE tenant_id=$1 AND credential_id=$2",
            "SELECT last_used_at_ms FROM control_node_credentials WHERE tenant_id=$1 AND credential_id=$2 FOR UPDATE"))
            .bind(snapshot.tenant_id.as_str()).bind(&snapshot.credential_id).fetch_one(&mut *tx).await.map_err(database_error)?;
        let existing:Option<String>=sqlx::query_scalar("SELECT storage_instance_id FROM control_node_storage_bindings WHERE tenant_id=$1 AND credential_id=$2")
            .bind(snapshot.tenant_id.as_str()).bind(&snapshot.credential_id).fetch_optional(&mut *tx).await.map_err(database_error)?;
        match existing {
            Some(existing) if existing != storage_instance_id => {
                return Err(HarnessError::policy(
                    "Node credential is bound to a different data directory",
                ));
            }
            None if last_used.is_some() => {
                return Err(HarnessError::policy(
                    "this used Node credential has no storage identity; revoke it and enroll again to connect, while prior cleanup remains unconfirmed",
                ));
            }
            None => {
                sqlx::query("INSERT INTO control_node_storage_bindings(tenant_id,credential_id,storage_instance_id,created_at_ms) VALUES($1,$2,$3,$4)")
                    .bind(snapshot.tenant_id.as_str()).bind(&snapshot.credential_id).bind(storage_instance_id).bind(number(now_ms)?)
                    .execute(&mut *tx).await.map_err(database_error)?;
            }
            Some(_) => {}
        }
        tx.commit().await.map_err(database_error)?;
        Ok(snapshot)
    }
}
