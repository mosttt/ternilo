use base64::{Engine as _, engine::general_purpose::STANDARD};
use ternilo_control::{
    ResourceAction, ResourceKind, event_references_attachment, resource_access_in,
};
use ternilo_protocol::{
    Attachment, HarnessError, SessionEvent, SessionId, TenantId, UserId, WorkspaceId,
};
use ternilo_storage::{Json, Transaction, database_error, set_user_scope};

use crate::{CloudStore, store::hex_digest};

impl CloudStore {
    /// Download one canonical file version, including files from archived sessions.
    pub async fn session_file_content_as(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        file_id: &str,
    ) -> Result<ternilo_protocol::SessionFileContent, HarnessError> {
        let locator = ternilo_protocol::SessionFileLocator::parse(file_id)?;
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ResourceAction::View,
        )
        .await?;
        let workspace_id: String = sqlx::query_scalar(
            "SELECT workspace_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let attachment =
            file_attachment_in(&mut transaction, tenant_id, session_id, &locator, file_id).await?;
        let bytes = if let Some(digest) = attachment.reference_digest() {
            let workspace = WorkspaceId::new(workspace_id);
            attachment_content_in(&mut transaction, tenant_id, &workspace, digest).await?
        } else {
            ternilo_local::inline_file_attachment_bytes(&attachment)?
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(ternilo_protocol::SessionFileContent {
            name: attachment.name,
            media_type: attachment.media_type,
            content_base64: STANDARD.encode(bytes),
        })
    }
    /// Read only an object already referenced by the canonical run or its session.
    pub async fn attachment_for_worker(
        &self,
        run: &crate::StartedRun,
        worker_id: &str,
        attachment: &Attachment,
        now_ms: u64,
    ) -> Result<Vec<u8>, HarnessError> {
        attachment.validate()?;
        let digest = attachment.reference_digest().ok_or_else(|| {
            HarnessError::invalid("Worker attachment download requires a retained reference")
        })?;
        let mut transaction = self.tenant_transaction(&run.claim.tenant_id).await?;
        crate::store::require_writer_in(&mut transaction, run, worker_id, Some(now_ms)).await?;
        if !run
            .claim
            .spec
            .attachments
            .iter()
            .any(|item| item.reference_digest() == Some(digest))
        {
            require_session_attachment_references_in(
                &mut transaction,
                &run.claim.tenant_id,
                &run.claim.session_id,
                std::slice::from_ref(attachment),
            )
            .await?;
        }
        let content = attachment_content_in(
            &mut transaction,
            &run.claim.tenant_id,
            &run.claim.spec.metadata.workspace_id,
            digest,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(content)
    }

    /// Resolve an owned workspace object after the caller has authorized it.
    pub async fn resolve_attachment(
        &self,
        tenant_id: &TenantId,
        workspace_id: &WorkspaceId,
        attachment: Attachment,
    ) -> Result<Attachment, HarnessError> {
        tenant_id.validate()?;
        workspace_id.validate()?;
        attachment.validate()?;
        if attachment.reference_digest().is_none() {
            return Ok(attachment);
        }
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        let resolved =
            resolve_attachment_in(&mut transaction, tenant_id, workspace_id, attachment).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(resolved)
    }

    pub async fn resolve_session_attachment_as(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        attachment: Attachment,
    ) -> Result<Attachment, HarnessError> {
        attachment.validate()?;
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        let access = resource_access_in(
            &mut transaction,
            actor_id,
            tenant_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await?;
        access.require(ResourceAction::View)?;
        set_user_scope(&mut transaction, &access.owner_user_id).await?;
        if !access.is_owner {
            require_session_attachment_references_in(
                &mut transaction,
                tenant_id,
                session_id,
                std::slice::from_ref(&attachment),
            )
            .await?;
        }
        let workspace: String = sqlx::query_scalar(
            "SELECT workspace_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 AND user_id=$3",
        ).bind(tenant_id.as_str()).bind(session_id.as_str()).bind(access.owner_user_id.as_str())
            .fetch_one(&mut *transaction).await.map_err(database_error)?;
        let resolved = resolve_attachment_in(
            &mut transaction,
            tenant_id,
            &WorkspaceId::new(workspace),
            attachment,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(resolved)
    }
}

/// Check existing canonical references before accepting a guest submission.
/// The new request must not authorize its own references by entering the queue.
pub(crate) async fn require_session_attachment_references_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    session_id: &SessionId,
    attachments: &[Attachment],
) -> Result<(), HarnessError> {
    for attachment in attachments {
        attachment.validate()?;
        let Some(digest) = attachment.reference_digest() else {
            continue;
        };
        let pattern = format!("%{digest}%");
        let events = sqlx::query_scalar::<_, Json<SessionEvent>>(
            "SELECT event FROM cloud_session_events
             WHERE tenant_id=$1 AND session_id=$2 AND event LIKE $3",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(&pattern)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
        if events
            .iter()
            .any(|event| event_references_attachment(&event.0, digest))
        {
            continue;
        }
        let accepted = sqlx::query_scalar::<_, Json<Attachment>>(
            "SELECT attachment FROM cloud_session_uploads
             WHERE tenant_id=$1 AND session_id=$2 AND attachment LIKE $3",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(pattern)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
        if !accepted
            .iter()
            .any(|item| item.0.reference_digest() == Some(digest))
        {
            return Err(HarnessError::policy(
                "attachment is not referenced by this session",
            ));
        }
    }
    Ok(())
}

async fn resolve_attachment_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    workspace_id: &WorkspaceId,
    attachment: Attachment,
) -> Result<Attachment, HarnessError> {
    let Some(digest) = attachment.reference_digest() else {
        return Ok(attachment);
    };
    let content = attachment_content_in(transaction, tenant_id, workspace_id, digest).await?;
    let resolved = if attachment.media_type.starts_with("image/") {
        format!(
            "data:{};base64,{}",
            attachment.media_type,
            STANDARD.encode(&content)
        )
    } else if let Ok(text) = String::from_utf8(content.clone()) {
        text
    } else {
        format!(
            "data:{};base64,{}",
            attachment.media_type,
            STANDARD.encode(content)
        )
    };
    Ok(Attachment {
        name: attachment.name,
        media_type: attachment.media_type,
        content: resolved,
    })
}

async fn attachment_content_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    workspace_id: &WorkspaceId,
    digest: &str,
) -> Result<Vec<u8>, HarnessError> {
    let content = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT content FROM cloud_attachment_objects
         WHERE tenant_id=$1 AND workspace_id=$2 AND digest=$3",
    )
    .bind(tenant_id.as_str())
    .bind(workspace_id.as_str())
    .bind(digest)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("cloud attachment object does not exist"))?;
    if hex_digest(&content) != digest {
        return Err(HarnessError::execution(
            "cloud attachment object failed digest verification",
        ));
    }
    Ok(content)
}

/// Persist accepted attachment identities independently of the mutable inbox.
pub(crate) async fn retain_submission_uploads_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    session_id: &SessionId,
    submission_id: &ternilo_protocol::SubmissionId,
    submitted_run_id: &ternilo_protocol::RunId,
    attachments: &[Attachment],
    created_at_ms: u64,
) -> Result<(), HarnessError> {
    for (index, attachment) in attachments.iter().enumerate() {
        sqlx::query("INSERT INTO cloud_session_uploads (tenant_id,session_id,submission_id,attachment_index,created_at_ms,submitted_run_id,attachment)
            VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (tenant_id,session_id,submission_id,attachment_index) DO NOTHING")
            .bind(tenant_id.as_str()).bind(session_id.as_str()).bind(submission_id.as_str())
            .bind(i64::try_from(index).map_err(|_| HarnessError::invalid("too many submission attachments"))?)
            .bind(crate::store::to_i64(created_at_ms, "accepted upload timestamp")?)
            .bind(submitted_run_id.as_str()).bind(Json(attachment))
            .execute(&mut **transaction).await.map_err(database_error)?;
    }
    Ok(())
}

async fn file_attachment_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    session_id: &SessionId,
    locator: &ternilo_protocol::SessionFileLocator,
    file_id: &str,
) -> Result<Attachment, HarnessError> {
    match locator {
        ternilo_protocol::SessionFileLocator::Submission {
            submission_id,
            attachment_index,
        } => sqlx::query_scalar::<_, Json<Attachment>>(
            "SELECT attachment FROM cloud_session_uploads
                WHERE tenant_id=$1 AND session_id=$2 AND submission_id=$3 AND attachment_index=$4",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(submission_id.as_str())
        .bind(i64::from(*attachment_index))
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .map(|value| value.0),
        ternilo_protocol::SessionFileLocator::Event { event_seq, .. } => {
            let event = sqlx::query_scalar::<_, Json<SessionEvent>>(
                "SELECT event FROM cloud_session_events
                WHERE tenant_id=$1 AND session_id=$2 AND seq=$3",
            )
            .bind(tenant_id.as_str())
            .bind(session_id.as_str())
            .bind(crate::store::to_i64(*event_seq, "file event sequence")?)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(database_error)?;
            event.and_then(|event| {
                ternilo_protocol::session_file_references(&event.0)
                    .into_iter()
                    .find(|item| item.id == file_id)
                    .map(|item| item.attachment.clone())
            })
        }
    }
    .ok_or_else(|| HarnessError::invalid("file does not exist in this session"))
}
