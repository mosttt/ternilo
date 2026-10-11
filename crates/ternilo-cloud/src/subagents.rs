use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{
    HarnessError, RunId, RunOutcome, RunSpec, SessionId, SubagentId, SubagentSessionMetadata,
    SubmissionContent, TenantId, UserId,
};
use ternilo_storage::{Backend, Json, Transaction, backend, for_update};

use crate::{
    CloudRunRecord, CloudRunState, CloudSessionRecord, CloudStore, StartedRun,
    store::{database_error, mode_str, permission_str, set_tenant},
};

#[derive(Clone, Debug, PartialEq)]
pub struct CloudSubagent {
    pub parent: CloudSessionRecord,
    pub child: CloudSessionRecord,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkerSubagentRun {
    pub state: CloudRunState,
    pub outcome: Option<RunOutcome>,
    pub error: Option<HarnessError>,
}

impl CloudStore {
    pub async fn create_subagent_for_worker(
        &self,
        worker_id: &str,
        parent: &StartedRun,
        child_session_id: &SessionId,
        metadata: &SubagentSessionMetadata,
        label: &str,
        now_ms: u64,
    ) -> Result<SessionId, HarnessError> {
        let mut transaction = self.begin().await?;
        let parent_run = require_parent_in(&mut transaction, parent, worker_id, now_ms).await?;
        crate::execution_admission::require_active_in(&mut transaction, parent, worker_id, now_ms)
            .await?;
        if parent_run
            .try_get::<String, _>("state")
            .map_err(database_error)?
            != "running"
        {
            return Err(HarnessError::policy(
                "a stopping parent cannot create a subagent",
            ));
        }
        let mut parent_session = parent_session_in(
            &mut transaction,
            &parent.claim.tenant_id,
            &parent.claim.spec.metadata.user_id,
            &parent.claim.session_id,
        )
        .await?;
        parent_session.model = crate::profile_model_snapshot(&parent.claim.spec.profile)?;
        let coordination =
            ternilo_protocol::CoordinationConfig::from_profile(&parent.claim.spec.profile)?;
        if coordination.mode == ternilo_protocol::CoordinationMode::Independent {
            return Err(HarnessError::policy("Independent coordination is active"));
        }
        parent_session.profile_plugins = parent.claim.spec.profile.plugins.clone();
        parent_session.permissions = parent.claim.spec.permissions;
        parent_session.mode = parent.claim.spec.mode;
        if let Some(role) = metadata.role {
            parent_session.model = accepted_role_model(&parent.claim.spec.profile, role)?;
            if coordination.role_read_only(role) {
                parent_session.permissions = ternilo_protocol::PermissionPreset::ReadOnly;
            }
            parent_session.profile_plugins = ternilo_kernel::compose_profiles([
                parent.claim.spec.profile.clone(),
                ternilo_protocol::Profile {
                    plugins: coordination.child_overrides(&parent.claim.spec.profile, role),
                },
            ])
            .plugins;
            for entry in &mut parent_session.profile_plugins {
                if entry.enabled && entry.kind == crate::BROKERED_MODEL_KIND {
                    entry.config = serde_json::json!({"snapshot":parent_session.model});
                }
            }
        }
        let child = create_subagent_in(
            &mut transaction,
            &parent_session,
            child_session_id,
            metadata,
            label,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(child)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve explicit parent lease, child identity, payload and time boundaries."
    )]
    pub async fn enqueue_subagent_for_worker(
        &self,
        worker_id: &str,
        parent: &StartedRun,
        child_session_id: &SessionId,
        spec: &RunSpec,
        input: &str,
        max_attempts: u32,
        now_ms: u64,
    ) -> Result<RunId, HarnessError> {
        let mut transaction = self.begin().await?;
        let parent_run = require_parent_in(&mut transaction, parent, worker_id, now_ms).await?;
        crate::execution_admission::require_active_in(&mut transaction, parent, worker_id, now_ms)
            .await?;
        if parent_run
            .try_get::<String, _>("state")
            .map_err(database_error)?
            != "running"
        {
            return Err(HarnessError::policy(
                "a stopping parent cannot enqueue a subagent run",
            ));
        }
        let child = child_session_in(&mut transaction, parent, child_session_id).await?;
        let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_runs WHERE tenant_id = $1 AND session_id = $2 AND state IN ('queued', 'leased', 'running', 'cancel_requested')")
            .bind(child.tenant_id.as_str()).bind(child.session_id.as_str()).fetch_one(&mut *transaction).await.map_err(database_error)?;
        let current: Option<String> = sqlx::query_scalar(
            "SELECT current_run_id FROM cloud_sessions WHERE tenant_id = $1 AND session_id = $2",
        )
        .bind(child.tenant_id.as_str())
        .bind(child.session_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if current.is_some() || active != 0 {
            return Err(HarnessError::conflict(
                "cloud Subagent Session already has an active run",
            ));
        }
        if spec.metadata.tenant_id != child.tenant_id
            || spec.metadata.user_id != child.user_id
            || spec.metadata.session_id != child.session_id
        {
            return Err(HarnessError::invalid(
                "cloud Subagent RunSpec does not match its canonical Session",
            ));
        }
        validate_child_spec(&child, parent, spec)?;
        let reservation = ternilo_control::ControlStore::reserve_quota_for_owner_in(
            &mut transaction,
            &parent.claim.actor_user_id,
            &child.user_id,
            &child.tenant_id,
            Some(spec.metadata.run_id.as_str()),
            child.reserved_model_tokens,
            std::time::Duration::from_hours(1),
            now_ms,
        )
        .await?;
        let compiled = crate::CompiledRun {
            automated_input: Some(ternilo_protocol::AutomatedInputSource::Subagent),
            actor_user_id: parent.claim.actor_user_id.clone(),
            authorization_session_id: parent.claim.authorization_session_id.clone(),
            spec: spec.clone(),
            reserved_model_tokens: child.reserved_model_tokens,
            priority: 0,
            max_attempts,
        };
        let request = ternilo_protocol::SessionSubmissionRequest {
            delivery: ternilo_protocol::SubmissionDelivery::Queue,
            run_id: Some(spec.metadata.run_id.clone()),
            content: SubmissionContent::Prompt {
                input: input.to_owned(),
            },
            references: Vec::new(),
            attachments: Vec::new(),
        };
        let receipt = Self::enqueue_session_submission_with_parent_in(
            &mut transaction,
            &compiled,
            &reservation.reservation_id,
            &request,
            Some(parent),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(receipt.submission.run_id)
    }

    pub async fn subagent_run_for_worker(
        &self,
        worker_id: &str,
        parent: &StartedRun,
        child_session_id: &SessionId,
        child_run_id: &RunId,
        now_ms: u64,
    ) -> Result<Option<WorkerSubagentRun>, HarnessError> {
        let mut transaction = self.begin().await?;
        if let Err(error) = require_parent_in(&mut transaction, parent, worker_id, now_ms).await {
            if error.code == ternilo_protocol::ErrorCode::PolicyDenied {
                return Ok(None);
            }
            return Err(error);
        }
        crate::run_lineage::require_accepted_dependency_in(
            &mut transaction,
            parent,
            child_session_id,
            child_run_id,
        )
        .await?;
        let row = sqlx::query("SELECT child.state, child.outcome, child.error FROM cloud_sessions AS session JOIN cloud_runs AS child
            ON child.tenant_id = session.tenant_id AND child.user_id = session.user_id AND child.session_id = session.session_id
            WHERE session.tenant_id = $1 AND session.user_id = $2 AND session.parent_session_id = $3
                AND session.session_id = $4 AND session.subagent_metadata IS NOT NULL AND child.run_id = $5")
            .bind(parent.claim.tenant_id.as_str()).bind(parent.claim.spec.metadata.user_id.as_str()).bind(parent.claim.session_id.as_str())
            .bind(child_session_id.as_str()).bind(child_run_id.as_str()).fetch_optional(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        row.map(|row| {
            Ok(WorkerSubagentRun {
                state: CloudRunState::parse(
                    &row.try_get::<String, _>("state").map_err(database_error)?,
                )?,
                outcome: row
                    .try_get::<Option<Json<RunOutcome>>, _>("outcome")
                    .map_err(database_error)?
                    .map(|value| value.0),
                error: row
                    .try_get::<Option<Json<HarnessError>>, _>("error")
                    .map_err(database_error)?
                    .map(|value| value.0),
            })
        })
        .transpose()
    }

    pub async fn cancel_subagent_for_worker(
        &self,
        worker_id: &str,
        parent: &StartedRun,
        child_session_id: &SessionId,
        child_run_id: &RunId,
        now_ms: u64,
    ) -> Result<CloudRunState, HarnessError> {
        let mut transaction = self.begin().await?;
        require_parent_in(&mut transaction, parent, worker_id, now_ms).await?;
        let child = child_session_in(&mut transaction, parent, child_session_id).await?;
        crate::run_lineage::require_accepted_dependency_in(
            &mut transaction,
            parent,
            child_session_id,
            child_run_id,
        )
        .await?;
        let state = Self::cancel_run_in(
            &mut transaction,
            &child.tenant_id,
            child_run_id,
            Some(&parent.claim.actor_user_id),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(state)
    }

    pub async fn cloud_subagent(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        parent_session_id: &SessionId,
        subagent_id: &SubagentId,
    ) -> Result<CloudSubagent, HarnessError> {
        tenant_id.validate()?;
        actor_id.validate()?;
        parent_session_id.validate()?;
        subagent_id.validate()?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            parent_session_id,
            ternilo_control::ResourceAction::View,
        )
        .await?;
        let user_id = &owner_id;
        let parent_row =
            sqlx::query("SELECT * FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2")
                .bind(tenant_id.as_str())
                .bind(parent_session_id.as_str())
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
        let parent = crate::store::decode_session(&parent_row)?;
        let query = if backend(&transaction) == Backend::Postgres {
            "SELECT session_id FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND parent_session_id = $3 AND CAST(subagent_metadata AS jsonb)->>'subagent_id' = $4 ORDER BY created_at_ms, session_id LIMIT 1"
        } else {
            "SELECT session_id FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND parent_session_id = $3 AND json_extract(subagent_metadata, '$.subagent_id') = $4 ORDER BY created_at_ms, session_id LIMIT 1"
        };
        let row = sqlx::query(query)
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .bind(parent_session_id.as_str())
            .bind(subagent_id.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            .ok_or_else(|| {
                HarnessError::invalid(format!("unknown cloud Subagent {subagent_id}"))
            })?;
        let child_id = SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        );
        crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            &child_id,
            ternilo_control::ResourceAction::View,
        )
        .await?;
        let child_row =
            sqlx::query("SELECT * FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2")
                .bind(tenant_id.as_str())
                .bind(child_id.as_str())
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
        let child = crate::store::decode_session(&child_row)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(CloudSubagent { parent, child })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve explicit parent lease, child identity, payload and time boundaries."
    )]
    pub async fn create_canonical_subagent_session(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        parent_session_id: &SessionId,
        child_session_id: &SessionId,
        metadata: SubagentSessionMetadata,
        label: String,
        now_ms: u64,
    ) -> Result<CloudSessionRecord, HarnessError> {
        if metadata.role.is_some() {
            return Err(HarnessError::policy(
                "agent roles require an accepted parent run",
            ));
        }
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            parent_session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        let parent =
            parent_session_in(&mut transaction, tenant_id, user_id, parent_session_id).await?;
        let child = create_subagent_in(
            &mut transaction,
            &parent,
            child_session_id,
            &metadata,
            &label,
            now_ms,
        )
        .await?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            parent_session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        self.owned_session(tenant_id, user_id, &child).await
    }

    pub async fn active_subagent_run(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
    ) -> Result<Option<CloudRunRecord>, HarnessError> {
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::View,
        )
        .await?;
        let user_id = &owner_id;
        let run_id = sqlx::query_scalar::<_, String>(
            "SELECT run_id FROM cloud_runs
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
               AND state IN ('queued', 'leased', 'running', 'cancel_requested')
             ORDER BY CASE WHEN state IN ('running', 'cancel_requested') THEN 0
                           WHEN state='leased' THEN 1 ELSE 2 END,
                      created_at_ms DESC, run_id DESC
             LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let Some(run_id) = run_id else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        transaction.commit().await.map_err(database_error)?;
        self.get_run(tenant_id, &RunId::new(run_id)).await.map(Some)
    }

    async fn owned_session(
        &self,
        tenant_id: &TenantId,
        user_id: &UserId,
        session_id: &SessionId,
    ) -> Result<CloudSessionRecord, HarnessError> {
        let session = self.get_session(tenant_id, session_id).await?;
        if session.user_id != *user_id {
            return Err(HarnessError::invalid("cloud Session does not exist"));
        }
        Ok(session)
    }
}

#[must_use]
pub fn worker_subagent_profile(
    parent: &RunSpec,
    child: &CloudSessionRecord,
) -> ternilo_protocol::Profile {
    let mut profile = ternilo_kernel::compose_profiles([
        parent.profile.clone(),
        ternilo_protocol::Profile {
            plugins: child.profile_plugins.clone(),
        },
    ]);
    for entry in &mut profile.plugins {
        if entry.enabled && entry.kind == crate::BROKERED_MODEL_KIND {
            entry.config = serde_json::json!({"snapshot":child.model});
        }
    }
    profile
}

fn validate_child_spec(
    child: &CloudSessionRecord,
    parent: &StartedRun,
    spec: &RunSpec,
) -> Result<(), HarnessError> {
    let coordination =
        ternilo_protocol::CoordinationConfig::from_profile(&parent.claim.spec.profile)?;
    if coordination.mode == ternilo_protocol::CoordinationMode::Independent {
        return Err(HarnessError::policy("Independent coordination is active"));
    }
    let permissions =
        if parent.claim.spec.permissions == ternilo_protocol::PermissionPreset::ReadOnly {
            ternilo_protocol::PermissionPreset::ReadOnly
        } else {
            child.permissions
        };
    let mode = if parent.claim.spec.mode == ternilo_protocol::SessionMode::Plan {
        ternilo_protocol::SessionMode::Plan
    } else {
        child.mode
    };
    // The canonical child was authorized at creation. Parent edits do not rewrite
    // that binding; the model gateway still checks current access on each call.
    if crate::profile_model_snapshot(&spec.profile)? != child.model
        || spec.permissions != permissions
        || spec.mode != mode
        || spec.profile != worker_subagent_profile(&parent.claim.spec, child)
    {
        return Err(HarnessError::policy(
            "subagent must retain its accepted model binding and permission ceiling",
        ));
    }
    Ok(())
}

fn accepted_role_model(
    profile: &ternilo_protocol::Profile,
    role: ternilo_protocol::AgentRole,
) -> Result<Option<ternilo_protocol::RunModelSnapshot>, HarnessError> {
    let config = ternilo_protocol::CoordinationConfig::from_profile(profile)?;
    match config.role(role).model.as_ref() {
        None | Some(ternilo_protocol::DefaultModelSelection::ProfileDefault) => {
            crate::profile_model_snapshot(profile)
        }
        Some(selection) => {
            let snapshot = config.resolved_models.get(&role).ok_or_else(|| {
                HarnessError::policy("agent role has no accepted Server model authorization")
            })?;
            if snapshot.selection() != *selection {
                return Err(HarnessError::policy(
                    "agent role model differs from the accepted role selection",
                ));
            }
            Ok(Some(snapshot.clone()))
        }
    }
}

fn to_i64(value: u64, label: &str) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::invalid(format!("{label} exceeds i64")))
}

async fn set_owner_scope(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
) -> Result<(), HarnessError> {
    set_tenant(transaction, tenant_id).await?;
    ternilo_storage::set_user_scope(transaction, user_id).await?;
    Ok(())
}

async fn parent_session_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    user: &UserId,
    session: &SessionId,
) -> Result<CloudSessionRecord, HarnessError> {
    set_owner_scope(transaction, tenant, user).await?;
    let query = for_update(
        transaction,
        "SELECT * FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
        "SELECT * FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 FOR UPDATE",
    );
    let row = sqlx::query(query)
        .bind(tenant.as_str())
        .bind(user.as_str())
        .bind(session.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("cloud parent Session does not exist"))?;
    crate::store::decode_session(&row)
}

async fn create_subagent_in(
    transaction: &mut Transaction,
    parent: &CloudSessionRecord,
    child: &SessionId,
    metadata: &SubagentSessionMetadata,
    label: &str,
    now_ms: u64,
) -> Result<SessionId, HarnessError> {
    child.validate()?;
    metadata.subagent_id.validate()?;
    if metadata.provider.trim().is_empty() || label.trim().is_empty() {
        return Err(HarnessError::invalid(
            "cloud Subagent provider and label must not be empty",
        ));
    }
    let query = if backend(transaction) == Backend::Postgres {
        "SELECT session_id, parent_session_id, subagent_metadata, subagent_role FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND (session_id = $3 OR (parent_session_id = $4 AND CAST(subagent_metadata AS jsonb)->>'subagent_id' = $5)) ORDER BY session_id LIMIT 1"
    } else {
        "SELECT session_id, parent_session_id, subagent_metadata, subagent_role FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND (session_id = $3 OR (parent_session_id = $4 AND json_extract(subagent_metadata, '$.subagent_id') = $5)) ORDER BY session_id LIMIT 1"
    };
    if let Some(row) = sqlx::query(query)
        .bind(parent.tenant_id.as_str())
        .bind(parent.user_id.as_str())
        .bind(child.as_str())
        .bind(parent.session_id.as_str())
        .bind(metadata.subagent_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
    {
        let prior = crate::store::decode_subagent_metadata(&row)?;
        let lineage: Option<String> = row.try_get("parent_session_id").map_err(database_error)?;
        if lineage.as_deref() != Some(parent.session_id.as_str())
            || prior.as_ref() != Some(metadata)
        {
            return Err(HarnessError::conflict(
                "cloud Subagent Session identity is already used",
            ));
        }
        return Ok(SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        ));
    }
    let mut stored_metadata = metadata.clone();
    let role = stored_metadata.role.take();
    sqlx::query("INSERT INTO cloud_sessions (tenant_id, session_id, user_id, project_id, workspace_id, agent_id,
        parent_session_id, subagent_metadata, title, permissions, model_snapshot,
        reserved_model_tokens, agent_preset, profile_plugins, mode, state, created_at_ms, updated_at_ms, subagent_role)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, 'idle', $16, $16, $17)")
        .bind(parent.tenant_id.as_str()).bind(child.as_str()).bind(parent.user_id.as_str()).bind(&parent.project_id)
        .bind(parent.workspace_id.as_str()).bind(parent.agent_id.as_str()).bind(parent.session_id.as_str()).bind(Json(&stored_metadata)).bind(label.trim())
        .bind(permission_str(parent.permissions)).bind(parent.model.as_ref().map(Json))
        .bind(to_i64(parent.reserved_model_tokens, "child token budget")?).bind(&parent.agent_preset).bind(Json(&parent.profile_plugins))
        .bind(mode_str(parent.mode)).bind(to_i64(now_ms, "child creation time")?).bind(role.map(ternilo_protocol::AgentRole::as_str)).execute(&mut **transaction).await.map_err(database_error)?;
    crate::execution_families::ensure_child_in(
        transaction,
        &parent.tenant_id,
        child,
        &parent.user_id,
        &parent.workspace_id,
        &parent.session_id,
        now_ms,
    )
    .await?;
    crate::telemetry::sync_session_telemetry_in(
        transaction,
        &parent.tenant_id,
        &parent.user_id,
        child,
        now_ms,
    )
    .await?;
    Ok(child.clone())
}

async fn require_parent_in(
    transaction: &mut Transaction,
    parent: &StartedRun,
    worker: &str,
    now_ms: u64,
) -> Result<AnyRow, HarnessError> {
    let row = crate::store::require_writer_in(transaction, parent, worker, Some(now_ms)).await?;
    let expiry: Option<i64> = row.try_get("lease_expires_at_ms").map_err(database_error)?;
    let now = to_i64(now_ms, "parent lease time")?;
    if expiry.is_none_or(|expiry| expiry <= now) {
        return Err(HarnessError::policy("cloud parent run lease has expired"));
    }
    Ok(row)
}

async fn child_session_in(
    transaction: &mut Transaction,
    parent: &StartedRun,
    child: &SessionId,
) -> Result<CloudSessionRecord, HarnessError> {
    let record = parent_session_in(
        transaction,
        &parent.claim.tenant_id,
        &parent.claim.spec.metadata.user_id,
        child,
    )
    .await?;
    if record.parent_session_id.as_ref() != Some(&parent.claim.session_id)
        || record.subagent.is_none()
    {
        return Err(HarnessError::policy(
            "canonical cloud Subagent Session does not belong to this parent",
        ));
    }
    Ok(record)
}
