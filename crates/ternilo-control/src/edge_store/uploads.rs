use sqlx::Row;
use ternilo_protocol::{AcceptedUploadChangeKind, HarnessError, SessionId, TenantId};
use ternilo_storage::{Transaction, lock};
use ternilo_transport::{
    AcceptedUploadBatch, ExecutorId, ExecutorScope, validate_upload_stream_id,
};

use super::{EdgeStore, database_error, from_i64, to_i64};

impl EdgeStore {
    pub async fn begin_upload_sync(
        &self,
        executor_id: &ExecutorId,
        scope: &ExecutorScope,
        stream_id: &str,
    ) -> Result<Option<u64>, HarnessError> {
        let mut transaction = self.transaction(&scope.tenant_id).await?;
        let cursor = self
            .begin_upload_sync_in_transaction(&mut transaction, executor_id, scope, stream_id)
            .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(cursor)
    }

    pub async fn begin_upload_sync_in_transaction(
        &self,
        transaction: &mut Transaction,
        executor_id: &ExecutorId,
        scope: &ExecutorScope,
        stream_id: &str,
    ) -> Result<Option<u64>, HarnessError> {
        validate_upload_stream_id(stream_id)?;
        require_upload_owner(transaction, executor_id, scope).await?;
        sqlx::query(
            "INSERT INTO control_edge_upload_streams
             (tenant_id, executor_id, owner_user_id, stream_id, last_seq) VALUES ($1, $2, $3, $4, 0)
             ON CONFLICT (tenant_id, executor_id) DO NOTHING",
        )
        .bind(scope.tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(scope.user_id.as_str())
        .bind(stream_id)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        upload_cursor(transaction, executor_id, scope, stream_id).await
    }

    pub async fn merge_uploads(
        &self,
        executor_id: &ExecutorId,
        batch: &AcceptedUploadBatch,
    ) -> Result<Option<u64>, HarnessError> {
        let mut transaction = self.transaction(&batch.scope.tenant_id).await?;
        let cursor = self
            .merge_uploads_in_transaction(&mut transaction, executor_id, batch)
            .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(cursor)
    }

    /// Cursor advancement and metadata projection commit together. A replayed
    /// batch never reintroduces an upload removed by a later session tombstone.
    pub async fn merge_uploads_in_transaction(
        &self,
        transaction: &mut Transaction,
        executor_id: &ExecutorId,
        batch: &AcceptedUploadBatch,
    ) -> Result<Option<u64>, HarnessError> {
        batch.validate()?;
        require_upload_owner(transaction, executor_id, &batch.scope).await?;
        let cursor =
            upload_cursor(transaction, executor_id, &batch.scope, &batch.stream_id).await?;
        let last = batch
            .changes
            .last()
            .expect("validated nonempty upload batch")
            .seq;
        if last <= cursor.unwrap_or(0) {
            return Ok(cursor);
        }
        if batch.after_seq != cursor {
            return Err(HarnessError::invalid(
                "upload delta does not continue the committed cursor",
            ));
        }
        for change in &batch.changes {
            match &change.kind {
                AcceptedUploadChangeKind::UploadAccepted { upload } => {
                    if is_deleted(
                        transaction,
                        &batch.scope.tenant_id,
                        executor_id,
                        &change.session_id,
                    )
                    .await?
                    {
                        return Err(HarnessError::policy(
                            "deleted Node session identity cannot be reused",
                        ));
                    }
                    let changed = sqlx::query(
                        "INSERT INTO control_edge_session_uploads
                         (tenant_id, executor_id, session_id, submission_id, attachment_index,
                          created_at_ms, submitted_run_id, name, media_type)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                         ON CONFLICT (tenant_id, executor_id, session_id, submission_id, attachment_index)
                         DO NOTHING",
                    ).bind(batch.scope.tenant_id.as_str()).bind(executor_id.as_str())
                        .bind(change.session_id.as_str()).bind(upload.submission_id.as_str())
                        .bind(i64::from(upload.attachment_index)).bind(to_i64(upload.created_at_ms, "upload timestamp")?)
                        .bind(upload.submitted_run_id.as_str()).bind(&upload.name).bind(&upload.media_type)
                        .execute(&mut **transaction).await.map_err(database_error)?.rows_affected();
                    if changed != 1 {
                        return Err(HarnessError::policy(
                            "upload stream repeated an immutable submission attachment",
                        ));
                    }
                }
                AcceptedUploadChangeKind::SessionDeleted => {
                    delete_session_metadata(
                        transaction,
                        executor_id,
                        &batch.scope.tenant_id,
                        &change.session_id,
                    )
                    .await?;
                }
            }
        }
        if batch
            .changes
            .iter()
            .any(|change| matches!(change.kind, AcceptedUploadChangeKind::SessionDeleted))
        {
            super::purge_deleted_session_mappings(transaction, &batch.scope.tenant_id, executor_id)
                .await?;
        }
        sqlx::query(
            "UPDATE control_edge_upload_streams SET last_seq = $3 WHERE tenant_id = $1 AND executor_id = $2",
        ).bind(batch.scope.tenant_id.as_str()).bind(executor_id.as_str()).bind(to_i64(last, "upload sequence")?)
            .execute(&mut **transaction).await.map_err(database_error)?;
        Ok(Some(last))
    }
}

async fn require_upload_owner(
    transaction: &mut Transaction,
    executor_id: &ExecutorId,
    scope: &ExecutorScope,
) -> Result<(), HarnessError> {
    executor_id.validate()?;
    scope.validate()?;
    lock(
        transaction,
        &format!("node-uploads:{}:{executor_id}", scope.tenant_id),
    )
    .await?;
    let found = sqlx::query_scalar::<_, String>(ternilo_storage::for_update(
        transaction,
        "SELECT executor_id FROM control_executors WHERE tenant_id = $1 AND executor_id = $2 AND owner_user_id = $3 AND state <> 'revoked'",
        "SELECT executor_id FROM control_executors WHERE tenant_id = $1 AND executor_id = $2 AND owner_user_id = $3 AND state <> 'revoked' FOR UPDATE",
    )).bind(scope.tenant_id.as_str()).bind(executor_id.as_str()).bind(scope.user_id.as_str())
        .fetch_optional(&mut **transaction).await.map_err(database_error)?;
    if found.is_none() {
        return Err(HarnessError::policy(
            "upload stream does not belong to an active enrolled computer",
        ));
    }
    Ok(())
}

async fn upload_cursor(
    transaction: &mut Transaction,
    executor_id: &ExecutorId,
    scope: &ExecutorScope,
    stream_id: &str,
) -> Result<Option<u64>, HarnessError> {
    let row = sqlx::query(
        "SELECT owner_user_id, stream_id, last_seq FROM control_edge_upload_streams WHERE tenant_id = $1 AND executor_id = $2",
    ).bind(scope.tenant_id.as_str()).bind(executor_id.as_str())
        .fetch_optional(&mut **transaction).await.map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("upload stream has not been initialized"))?;
    if row
        .try_get::<String, _>("owner_user_id")
        .map_err(database_error)?
        != scope.user_id.as_str()
        || row
            .try_get::<String, _>("stream_id")
            .map_err(database_error)?
            != stream_id
    {
        return Err(HarnessError::policy(
            "computer upload data identity changed; enroll the new data directory as a new computer",
        ));
    }
    let sequence = from_i64(
        row.try_get("last_seq").map_err(database_error)?,
        "upload sequence",
    )?;
    Ok((sequence != 0).then_some(sequence))
}

pub(crate) async fn session_deleted(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    executor_id: &ExecutorId,
    session_id: &SessionId,
) -> Result<bool, HarnessError> {
    lock(
        transaction,
        &format!("node-uploads:{tenant_id}:{executor_id}"),
    )
    .await?;
    is_deleted(transaction, tenant_id, executor_id, session_id).await
}

async fn is_deleted(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    executor_id: &ExecutorId,
    session_id: &SessionId,
) -> Result<bool, HarnessError> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT session_id FROM control_edge_deleted_sessions WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3",
    ).bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(session_id.as_str())
        .fetch_optional(&mut **transaction).await.map_err(database_error)?.is_some())
}

async fn delete_session_metadata(
    transaction: &mut Transaction,
    executor_id: &ExecutorId,
    tenant_id: &TenantId,
    session_id: &SessionId,
) -> Result<(), HarnessError> {
    for statement in [
        "INSERT INTO control_edge_deleted_sessions (tenant_id,executor_id,session_id) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING",
        "DELETE FROM control_edge_session_uploads WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3",
        "DELETE FROM control_edge_events WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3",
    ] {
        sqlx::query(statement)
            .bind(tenant_id.as_str())
            .bind(executor_id.as_str())
            .bind(session_id.as_str())
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
    }
    Ok(())
}
