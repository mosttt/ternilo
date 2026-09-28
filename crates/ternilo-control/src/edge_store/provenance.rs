use std::collections::BTreeSet;

use sqlx::Row;
use ternilo_protocol::{
    HarnessError, InputAuthor, InputProvenance, SessionEvent, SessionEventKind, SessionId,
    SessionSubmission, SubagentId, SubmissionId, TenantId, UserMessageSource,
};
use ternilo_storage::Transaction;
use ternilo_transport::ExecutorId;

use super::{EdgeStore, database_error, to_i64, validate_route};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProvenanceClassification {
    Verified,
    UnverifiedAccount,
    Unclaimed,
}

impl ProvenanceClassification {
    fn require_verified_account(self) -> Result<(), HarnessError> {
        if self == Self::UnverifiedAccount {
            return Err(HarnessError::policy(
                "Node claimed a platform author without an accepted input",
            ));
        }
        Ok(())
    }
}

impl EdgeStore {
    /// Keep the authenticated mapping lineage after a parent session is deleted.
    pub async fn record_session_provenance_context_in_transaction(
        transaction: &mut Transaction,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        parent_session_id: Option<&SessionId>,
        subagent_id: Option<&SubagentId>,
    ) -> Result<(), HarnessError> {
        validate_route(tenant_id, executor_id)?;
        session_id.validate()?;
        if let Some(parent) = parent_session_id {
            parent.validate()?;
            if parent == session_id {
                return Err(HarnessError::invalid(
                    "session provenance cannot reference itself",
                ));
            }
        }
        if let Some(subagent) = subagent_id {
            subagent.validate()?;
            if parent_session_id.is_none() {
                return Err(HarnessError::invalid(
                    "subagent provenance requires its parent session",
                ));
            }
        }
        sqlx::query(
            "INSERT INTO control_edge_session_provenance
             (tenant_id, executor_id, session_id, parent_session_id, subagent_id)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (tenant_id, executor_id, session_id) DO NOTHING",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(session_id.as_str())
        .bind(parent_session_id.map(SessionId::as_str))
        .bind(subagent_id.map(SubagentId::as_str))
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        let row = sqlx::query(
            "SELECT parent_session_id, subagent_id FROM control_edge_session_provenance
             WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(session_id.as_str())
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
        if row
            .try_get::<Option<String>, _>("parent_session_id")
            .map_err(database_error)?
            .as_deref()
            != parent_session_id.map(SessionId::as_str)
            || row
                .try_get::<Option<String>, _>("subagent_id")
                .map_err(database_error)?
                .as_deref()
                != subagent_id.map(SubagentId::as_str)
        {
            return Err(HarnessError::conflict(
                "session provenance cannot change its parent or subagent identity",
            ));
        }
        Ok(())
    }

    /// Record the authenticated input in the same transaction as its delivery command.
    pub async fn record_input_provenance_in_transaction(
        transaction: &mut Transaction,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        target_subagent_id: Option<&SubagentId>,
        provenance: &InputProvenance,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_route(tenant_id, executor_id)?;
        session_id.validate()?;
        provenance.validate()?;
        if let Some(subagent) = target_subagent_id {
            subagent.validate()?;
        }
        if !matches!(provenance.author, InputAuthor::Account { .. }) {
            return Err(HarnessError::policy(
                "remote input must identify an authenticated platform account",
            ));
        }
        let document = serde_json::to_string(provenance).map_err(|error| {
            HarnessError::execution(format!("encode input provenance: {error}"))
        })?;
        sqlx::query(
            "INSERT INTO control_edge_input_provenance
             (tenant_id, executor_id, input_id, session_id, target_subagent_id, provenance_json, created_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (tenant_id, executor_id, input_id) DO NOTHING",
        ).bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(provenance.input_id.as_str())
            .bind(session_id.as_str()).bind(target_subagent_id.map(SubagentId::as_str))
            .bind(&document).bind(to_i64(now_ms, "input acceptance timestamp")?)
            .execute(&mut **transaction).await.map_err(database_error)?;
        let row = sqlx::query(
            "SELECT session_id, target_subagent_id, provenance_json FROM control_edge_input_provenance
             WHERE tenant_id=$1 AND executor_id=$2 AND input_id=$3",
        ).bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(provenance.input_id.as_str())
            .fetch_one(&mut **transaction).await.map_err(database_error)?;
        if row
            .try_get::<String, _>("session_id")
            .map_err(database_error)?
            != session_id.as_str()
            || row
                .try_get::<Option<String>, _>("target_subagent_id")
                .map_err(database_error)?
                .as_deref()
                != target_subagent_id.map(SubagentId::as_str)
            || row
                .try_get::<String, _>("provenance_json")
                .map_err(database_error)?
                != document
        {
            return Err(HarnessError::conflict(
                "accepted input identity cannot change its author or destination",
            ));
        }
        Ok(())
    }

    pub(crate) async fn verify_account_model_input_in_transaction(
        transaction: &mut Transaction,
        tenant: &TenantId,
        executor: &ExecutorId,
        session: &SessionId,
        provenance: &InputProvenance,
    ) -> Result<(), HarnessError> {
        if !matches!(provenance.author, InputAuthor::Account { .. }) {
            return Err(HarnessError::policy(
                "account model input must identify an accepted account",
            ));
        }
        classify_input_provenance(
            transaction,
            tenant,
            executor,
            session,
            Some(provenance),
            None,
        )
        .await?
        .require_verified_account()
    }

    pub async fn verify_input_provenance(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        provenance: &InputProvenance,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.transaction(tenant_id).await?;
        Self::classify_message_provenance_in_transaction(
            &mut transaction,
            tenant_id,
            executor_id,
            session_id,
            Some(provenance),
            None,
        )
        .await?
        .require_verified_account()?;
        transaction.commit().await.map_err(database_error)
    }

    pub(super) async fn classify_message_provenance_in_transaction(
        transaction: &mut Transaction,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        provenance: Option<&InputProvenance>,
        source: Option<&UserMessageSource>,
    ) -> Result<ProvenanceClassification, HarnessError> {
        let source_input = match source {
            Some(UserMessageSource::Submission { submission_id, .. }) => Some(submission_id),
            _ => None,
        };
        classify_input_provenance(
            transaction,
            tenant_id,
            executor_id,
            session_id,
            provenance,
            source_input,
        )
        .await
    }

    pub async fn verify_submission_provenance(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        submission: &SessionSubmission,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.transaction(tenant_id).await?;
        classify_input_provenance(
            &mut transaction,
            tenant_id,
            executor_id,
            session_id,
            submission.provenance.as_ref(),
            Some(&submission.id),
        )
        .await?
        .require_verified_account()?;
        transaction.commit().await.map_err(database_error)
    }

    /// Clear account labels that this route cannot authenticate without changing Node data.
    pub async fn project_submission_provenance(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        submission: &mut SessionSubmission,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.transaction(tenant_id).await?;
        let classification = classify_input_provenance(
            &mut transaction,
            tenant_id,
            executor_id,
            session_id,
            submission.provenance.as_ref(),
            Some(&submission.id),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        if classification == ProvenanceClassification::UnverifiedAccount {
            submission.provenance = None;
        }
        Ok(())
    }

    pub async fn project_event_provenance(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        events: &mut [SessionEvent],
    ) -> Result<(), HarnessError> {
        let mut transaction = self.transaction(tenant_id).await?;
        Self::project_event_provenance_in_transaction(
            &mut transaction,
            tenant_id,
            executor_id,
            session_id,
            events,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub(crate) async fn project_event_provenance_in_transaction(
        transaction: &mut Transaction,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        events: &mut [SessionEvent],
    ) -> Result<(), HarnessError> {
        for event in events {
            if let SessionEventKind::UserMessage {
                provenance, source, ..
            } = &mut event.kind
            {
                let classification = Self::classify_message_provenance_in_transaction(
                    transaction,
                    tenant_id,
                    executor_id,
                    session_id,
                    provenance.as_ref(),
                    source.as_ref(),
                )
                .await?;
                if classification == ProvenanceClassification::UnverifiedAccount {
                    *provenance = None;
                }
            }
        }
        Ok(())
    }
}

async fn classify_input_provenance(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    executor_id: &ExecutorId,
    session_id: &SessionId,
    provenance: Option<&InputProvenance>,
    source_input: Option<&SubmissionId>,
) -> Result<ProvenanceClassification, HarnessError> {
    validate_route(tenant_id, executor_id)?;
    session_id.validate()?;
    if let Some(provenance) = provenance {
        provenance.validate()?;
    }
    if let (Some(source_id), Some(provenance)) = (source_input, provenance)
        && source_id != &provenance.input_id
    {
        return Err(HarnessError::policy(
            "Node changed the accepted input identity",
        ));
    }
    let Some(input_id) = provenance.map(|value| &value.input_id).or(source_input) else {
        return Ok(ProvenanceClassification::Unclaimed);
    };
    input_id.validate()?;
    let row = sqlx::query(
        "SELECT session_id, target_subagent_id, provenance_json FROM control_edge_input_provenance
         WHERE tenant_id=$1 AND executor_id=$2 AND input_id=$3",
    )
    .bind(tenant_id.as_str())
    .bind(executor_id.as_str())
    .bind(input_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    if let Some(row) = row {
        let accepted: InputProvenance = serde_json::from_str(
            &row.try_get::<String, _>("provenance_json")
                .map_err(database_error)?,
        )
        .map_err(|error| {
            HarnessError::execution(format!("decode accepted input provenance: {error}"))
        })?;
        if Some(&accepted) != provenance {
            return Err(HarnessError::policy(
                "Node omitted or changed the accepted input author",
            ));
        }
        let origin: String = row.try_get("session_id").map_err(database_error)?;
        let target: Option<String> = row.try_get("target_subagent_id").map_err(database_error)?;
        if !delivery_context_allows(
            transaction,
            tenant_id,
            executor_id,
            session_id,
            &origin,
            target.as_deref(),
        )
        .await?
        {
            return Err(HarnessError::policy(
                "Node used an accepted input in an unrelated session",
            ));
        }
        Ok(ProvenanceClassification::Verified)
    } else if provenance.is_some_and(|value| matches!(value.author, InputAuthor::Account { .. })) {
        Ok(ProvenanceClassification::UnverifiedAccount)
    } else {
        Ok(ProvenanceClassification::Unclaimed)
    }
}

async fn delivery_context_allows(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    executor_id: &ExecutorId,
    session_id: &SessionId,
    origin: &str,
    target_subagent: Option<&str>,
) -> Result<bool, HarnessError> {
    let mut current = session_id.as_str().to_owned();
    let mut visited = BTreeSet::new();
    while visited.insert(current.clone()) {
        if target_subagent.is_none() && current == origin {
            return Ok(true);
        }
        let Some(row) = sqlx::query(
            "SELECT parent_session_id, subagent_id FROM control_edge_session_provenance
             WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(&current)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        else {
            return Ok(false);
        };
        let parent: Option<String> = row.try_get("parent_session_id").map_err(database_error)?;
        let subagent: Option<String> = row.try_get("subagent_id").map_err(database_error)?;
        if let Some(subagent) = subagent {
            return Ok(
                target_subagent == Some(subagent.as_str()) && parent.as_deref() == Some(origin)
            );
        }
        let Some(parent) = parent else {
            return Ok(false);
        };
        current = parent;
    }
    Ok(false)
}
