use std::collections::BTreeSet;

use ternilo_protocol::{HarnessError, SessionId, TenantId};
use ternilo_storage::{Transaction, database_error};
use ternilo_transport::ExecutorId;

pub(crate) async fn purge_deleted_session_mappings(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    executor_id: &ExecutorId,
) -> Result<BTreeSet<SessionId>, HarnessError> {
    let deleted = sqlx::query_scalar::<_, String>(
        "SELECT session_id FROM control_edge_deleted_sessions WHERE tenant_id=$1 AND executor_id=$2",
    )
    .bind(tenant_id.as_str())
    .bind(executor_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    if deleted.is_empty() {
        return Ok(BTreeSet::new());
    }
    for statement in [
        "DELETE FROM control_resource_shares WHERE tenant_id=$1 AND resource_kind='session' AND resource_id IN
         (SELECT session_id FROM control_edge_sessions WHERE tenant_id=$1 AND executor_id=$2 AND node_session_id IN
          (SELECT session_id FROM control_edge_deleted_sessions WHERE tenant_id=$1 AND executor_id=$2))",
        "DELETE FROM control_resource_group_shares WHERE tenant_id=$1 AND resource_kind='session' AND resource_id IN
         (SELECT session_id FROM control_edge_sessions WHERE tenant_id=$1 AND executor_id=$2 AND node_session_id IN
          (SELECT session_id FROM control_edge_deleted_sessions WHERE tenant_id=$1 AND executor_id=$2))",
        "DELETE FROM control_resource_fork_group_sources WHERE tenant_id=$1 AND session_id IN
         (SELECT session_id FROM control_edge_sessions WHERE tenant_id=$1 AND executor_id=$2 AND node_session_id IN
          (SELECT session_id FROM control_edge_deleted_sessions WHERE tenant_id=$1 AND executor_id=$2))",
        "DELETE FROM control_edge_sessions WHERE tenant_id=$1 AND executor_id=$2 AND node_session_id IN
         (SELECT session_id FROM control_edge_deleted_sessions WHERE tenant_id=$1 AND executor_id=$2)",
    ] {
        sqlx::query(statement)
            .bind(tenant_id.as_str())
            .bind(executor_id.as_str())
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
    }
    Ok(deleted.into_iter().map(SessionId::new).collect())
}
