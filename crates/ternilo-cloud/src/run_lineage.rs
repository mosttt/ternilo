use serde::{Deserialize, Serialize};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{AcceptedSubagentRun, HarnessError, RunId, SessionId, TenantId, UserId};
use ternilo_storage::{Transaction, set_tenant_scope, set_user_scope};

use crate::{
    CloudStore, CompiledRun, StartedRun,
    store::{database_error, from_i64, to_i64},
};

/// Immutable scheduling ancestry, independent of removable run and session history.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudRunLineage {
    pub tenant_id: TenantId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub owner_user_id: UserId,
    pub actor_user_id: UserId,
    pub root_run_id: RunId,
    pub parent_run_id: Option<RunId>,
    pub parent_lease_token: Option<u64>,
    pub parent_writer_fencing_token: Option<u64>,
    pub depth: u32,
    pub created_at_ms: u64,
}

impl CloudStore {
    pub async fn run_lineage(
        &self,
        tenant_id: &TenantId,
        owner_user_id: &UserId,
        run_id: &RunId,
    ) -> Result<Option<CloudRunLineage>, HarnessError> {
        tenant_id.validate()?;
        owner_user_id.validate()?;
        run_id.validate()?;
        let mut transaction = self.owner_transaction(tenant_id, owner_user_id).await?;
        let result = lineage_in(&mut transaction, tenant_id, owner_user_id, run_id).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(result)
    }

    /// Validate an accepted automatic child against the current parent execution generation.
    pub async fn accepted_subagent_dependency_for_worker(
        &self,
        worker_id: &str,
        parent: &StartedRun,
        child_session_id: &SessionId,
        child_run_id: &RunId,
        now_ms: u64,
    ) -> Result<AcceptedSubagentRun, HarnessError> {
        child_session_id.validate()?;
        child_run_id.validate()?;
        let mut transaction = self.begin().await?;
        let current =
            crate::store::require_writer_in(&mut transaction, parent, worker_id, Some(now_ms))
                .await?;
        let expiry: Option<i64> = current
            .try_get("lease_expires_at_ms")
            .map_err(database_error)?;
        let now = to_i64(now_ms, "dependency parent lease time")?;
        if current
            .try_get::<String, _>("state")
            .map_err(database_error)?
            != "running"
            || expiry.is_none_or(|expiry| expiry <= now)
        {
            return Err(HarnessError::policy(
                "dependency parent is no longer running",
            ));
        }
        let accepted = require_accepted_dependency_in(
            &mut transaction,
            parent,
            child_session_id,
            child_run_id,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(accepted)
    }
}

pub(crate) async fn require_accepted_dependency_in(
    transaction: &mut Transaction,
    parent: &StartedRun,
    child_session_id: &SessionId,
    child_run_id: &RunId,
) -> Result<AcceptedSubagentRun, HarnessError> {
    let tenant = &parent.claim.tenant_id;
    let owner = &parent.claim.spec.metadata.user_id;
    let lineage = lineage_in(transaction, tenant, owner, child_run_id)
        .await?
        .ok_or_else(|| HarnessError::policy("accepted subagent dependency does not exist"))?;
    if lineage.session_id != *child_session_id
        || lineage.actor_user_id != parent.claim.actor_user_id
        || lineage.parent_run_id.as_ref() != Some(&parent.claim.run_id)
        || lineage.parent_lease_token != Some(parent.claim.lease_token)
        || lineage.parent_writer_fencing_token != Some(parent.fencing_token)
    {
        return Err(HarnessError::policy(
            "accepted subagent dependency belongs to a different parent execution",
        ));
    }
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2
         AND session_id=$3 AND user_id=$4 AND actor_user_id=$5",
    )
    .bind(tenant.as_str())
    .bind(child_run_id.as_str())
    .bind(child_session_id.as_str())
    .bind(owner.as_str())
    .bind(parent.claim.actor_user_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    if exists != 1 {
        return Err(HarnessError::policy(
            "accepted subagent run is no longer available",
        ));
    }
    Ok(AcceptedSubagentRun {
        session_id: lineage.session_id,
        run_id: lineage.run_id,
    })
}

pub(crate) async fn record_in(
    transaction: &mut Transaction,
    compiled: &CompiledRun,
    parent: Option<&StartedRun>,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let metadata = &compiled.spec.metadata;
    set_tenant_scope(transaction, &metadata.tenant_id).await?;
    set_user_scope(transaction, &metadata.user_id).await?;
    let (root_run_id, depth) = if let Some(parent) = parent {
        if parent.claim.tenant_id != metadata.tenant_id
            || parent.claim.spec.metadata.user_id != metadata.user_id
            || parent.claim.actor_user_id != compiled.actor_user_id
            || parent.claim.run_id == metadata.run_id
        {
            return Err(HarnessError::policy(
                "subagent scheduling ancestry must retain its parent scope",
            ));
        }
        let ancestor = lineage_in(
            transaction,
            &metadata.tenant_id,
            &metadata.user_id,
            &parent.claim.run_id,
        )
        .await?
        .ok_or_else(|| HarnessError::policy("parent run has no accepted scheduling ancestry"))?;
        if ancestor.actor_user_id != compiled.actor_user_id {
            return Err(HarnessError::policy(
                "subagent scheduling ancestry has a different actor",
            ));
        }
        let depth = ancestor
            .depth
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("subagent scheduling depth overflow"))?;
        (ancestor.root_run_id, depth)
    } else {
        (metadata.run_id.clone(), 0)
    };
    sqlx::query(
        "INSERT INTO cloud_run_lineage
         (tenant_id,run_id,session_id,owner_user_id,actor_user_id,root_run_id,parent_run_id,
          parent_lease_token,parent_writer_fencing_token,depth,created_at_ms)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(metadata.tenant_id.as_str())
    .bind(metadata.run_id.as_str())
    .bind(metadata.session_id.as_str())
    .bind(metadata.user_id.as_str())
    .bind(compiled.actor_user_id.as_str())
    .bind(root_run_id.as_str())
    .bind(parent.map(|parent| parent.claim.run_id.as_str()))
    .bind(
        parent
            .map(|parent| to_i64(parent.claim.lease_token, "parent run lease token"))
            .transpose()?,
    )
    .bind(
        parent
            .map(|parent| to_i64(parent.fencing_token, "parent writer fencing token"))
            .transpose()?,
    )
    .bind(i64::from(depth))
    .bind(to_i64(now_ms, "run lineage timestamp")?)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn lineage_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    owner: &UserId,
    run: &RunId,
) -> Result<Option<CloudRunLineage>, HarnessError> {
    sqlx::query(
        "SELECT * FROM cloud_run_lineage WHERE tenant_id=$1 AND owner_user_id=$2 AND run_id=$3",
    )
    .bind(tenant.as_str())
    .bind(owner.as_str())
    .bind(run.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .as_ref()
    .map(decode)
    .transpose()
}

fn decode(row: &AnyRow) -> Result<CloudRunLineage, HarnessError> {
    Ok(CloudRunLineage {
        tenant_id: TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        ),
        run_id: RunId::new(row.try_get::<String, _>("run_id").map_err(database_error)?),
        session_id: SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        ),
        owner_user_id: UserId::new(
            row.try_get::<String, _>("owner_user_id")
                .map_err(database_error)?,
        ),
        actor_user_id: UserId::new(
            row.try_get::<String, _>("actor_user_id")
                .map_err(database_error)?,
        ),
        root_run_id: RunId::new(
            row.try_get::<String, _>("root_run_id")
                .map_err(database_error)?,
        ),
        parent_run_id: row
            .try_get::<Option<String>, _>("parent_run_id")
            .map_err(database_error)?
            .map(RunId::new),
        parent_lease_token: row
            .try_get::<Option<i64>, _>("parent_lease_token")
            .map_err(database_error)?
            .map(|value| from_i64(value, "parent run lease token"))
            .transpose()?,
        parent_writer_fencing_token: row
            .try_get::<Option<i64>, _>("parent_writer_fencing_token")
            .map_err(database_error)?
            .map(|value| from_i64(value, "parent writer fencing token"))
            .transpose()?,
        depth: u32::try_from(row.try_get::<i64, _>("depth").map_err(database_error)?)
            .map_err(|_| HarnessError::execution("invalid persisted run lineage depth"))?,
        created_at_ms: from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "run lineage timestamp",
        )?,
    })
}
