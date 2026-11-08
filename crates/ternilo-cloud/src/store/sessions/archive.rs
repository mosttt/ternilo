use std::collections::{BTreeSet, VecDeque};

use super::{
    CloudSessionRecord, CloudStore, database_error, decode_session, select_session, set_tenant,
    to_i64,
};
use ternilo_control::ResourceAction;
use ternilo_protocol::{HarnessError, RunId, SessionId, TenantId, UserId};
use ternilo_storage::Transaction;

// Lock ancestors before discovering their children so concurrent subagent creation cannot escape.
pub(super) async fn family_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    owner: &UserId,
    root: &SessionId,
) -> Result<Vec<SessionId>, HarnessError> {
    let mut pending = VecDeque::from([root.clone()]);
    let mut visited = BTreeSet::new();
    let mut family = Vec::new();
    while let Some(id) = pending.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        crate::store::lock_session_in(tx, tenant, &id).await?;
        let children = sqlx::query_scalar::<_, String>(
            "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND user_id=$2
             AND parent_session_id=$3 AND subagent_metadata IS NOT NULL ORDER BY session_id",
        )
        .bind(tenant.as_str())
        .bind(owner.as_str())
        .bind(id.as_str())
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?;
        pending.extend(children.into_iter().map(SessionId::new));
        family.push(id);
    }
    Ok(family)
}

impl CloudStore {
    pub async fn archive_session(
        &self,
        tenant: &TenantId,
        actor: &UserId,
        root: &SessionId,
        now_ms: u64,
    ) -> Result<CloudSessionRecord, HarnessError> {
        let now = to_i64(now_ms, "cloud session archive timestamp")?;
        let mut tx = self.begin().await?;
        let owner =
            crate::sharing::session_owner_in(&mut tx, tenant, actor, root, ResourceAction::Delete)
                .await?;
        set_tenant(&mut tx, tenant).await?;
        for id in family_in(&mut tx, tenant, &owner, root).await? {
            // Park the FIFO before cancellation, which can otherwise promote another queued run.
            sqlx::query(
                "INSERT INTO cloud_session_inboxes (tenant_id,user_id,session_id,paused,error,next_position,updated_at_ms)
                 VALUES ($1,$2,$3,1,NULL,0,$4)
                 ON CONFLICT (tenant_id,user_id,session_id) DO UPDATE
                 SET paused=1,restart_after_run_id=NULL,updated_at_ms=EXCLUDED.updated_at_ms",
            ).bind(tenant.as_str()).bind(owner.as_str()).bind(id.as_str()).bind(now)
                .execute(&mut *tx).await.map_err(database_error)?;
            let runs = sqlx::query_scalar::<_, String>(
                "SELECT run_id FROM cloud_runs WHERE tenant_id=$1 AND session_id=$2
                 AND state IN ('queued','leased','running','cancel_requested') ORDER BY run_id",
            )
            .bind(tenant.as_str())
            .bind(id.as_str())
            .fetch_all(&mut *tx)
            .await
            .map_err(database_error)?;
            for run in runs {
                Self::cancel_run_in(&mut tx, tenant, &RunId::new(run), Some(actor), now_ms).await?;
            }
            let changed = sqlx::query(
                "UPDATE cloud_sessions SET archived_at_ms=$4,updated_at_ms=$4
                 WHERE tenant_id=$1 AND session_id=$2 AND user_id=$3 AND archived_at_ms IS NULL",
            )
            .bind(tenant.as_str())
            .bind(id.as_str())
            .bind(owner.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?
            .rows_affected();
            if changed > 0 {
                crate::sharing::audit_session_in(
                    &mut tx,
                    tenant,
                    actor,
                    &id,
                    &owner,
                    ResourceAction::Delete,
                    now_ms,
                )
                .await?;
            }
        }
        let result = decode_session(&select_session(&mut tx, tenant, root).await?)?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }

    pub async fn restore_session(
        &self,
        tenant: &TenantId,
        actor: &UserId,
        root: &SessionId,
        now_ms: u64,
    ) -> Result<CloudSessionRecord, HarnessError> {
        let mut tx = self.begin().await?;
        let owner =
            crate::sharing::session_owner_in(&mut tx, tenant, actor, root, ResourceAction::Delete)
                .await?;
        set_tenant(&mut tx, tenant).await?;
        crate::store::lock_session_in(&mut tx, tenant, root).await?;
        let original = decode_session(&select_session(&mut tx, tenant, root).await?)?;
        if original.archived_at_ms.is_some() {
            for id in family_in(&mut tx, tenant, &owner, root).await? {
                let changed = sqlx::query(
                    "UPDATE cloud_sessions SET archived_at_ms=NULL
                     WHERE tenant_id=$1 AND session_id=$2 AND user_id=$3 AND archived_at_ms IS NOT NULL",
                ).bind(tenant.as_str()).bind(id.as_str()).bind(owner.as_str())
                    .execute(&mut *tx).await.map_err(database_error)?.rows_affected();
                if changed > 0 {
                    crate::sharing::audit_session_in(
                        &mut tx,
                        tenant,
                        actor,
                        &id,
                        &owner,
                        ResourceAction::Delete,
                        now_ms,
                    )
                    .await?;
                }
            }
        }
        let result = decode_session(&select_session(&mut tx, tenant, root).await?)?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }
}
