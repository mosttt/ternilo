use super::{
    CloudStore, HarnessError, Row, RunId, SessionId, StartedRun, SubmissionContent,
    SubmissionDelivery, TenantId, UserId, database_error, decode_submission, promote_head,
    require_owned_session, set_scope, to_i64,
};
use ternilo_protocol::{RunSpec, SteeringInput, UserMessageSource};
use ternilo_storage::{Json, Transaction};

fn conversational(content: &SubmissionContent) -> bool {
    matches!(content, SubmissionContent::Prompt { input } if !input.trim_start().starts_with('/'))
}

pub(super) async fn claim_batch_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    user: &UserId,
    session: &SessionId,
    target: &RunId,
    now: i64,
) -> Result<(), HarnessError> {
    let rows = sqlx::query(
        "SELECT submission.submission_id, submission.run_id, submission.content, run.spec
        FROM cloud_session_submissions AS submission JOIN cloud_runs AS run
          ON run.tenant_id=submission.tenant_id AND run.run_id=submission.run_id
        WHERE submission.tenant_id=$1 AND submission.user_id=$2 AND submission.session_id=$3
          AND (submission.run_id=$4 OR submission.placement='queued')
        ORDER BY submission.fifo_position",
    )
    .bind(tenant.as_str())
    .bind(user.as_str())
    .bind(session.as_str())
    .bind(target.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    let Some(head) = rows.first() else {
        return Ok(());
    };
    if head.get::<String, _>("run_id") != target.as_str()
        || !conversational(&head.get::<Json<SubmissionContent>, _>("content").0)
    {
        return Ok(());
    }
    let head = head.get::<Json<RunSpec>, _>("spec").0;
    for row in rows.iter().skip(1) {
        let content = row.get::<Json<SubmissionContent>, _>("content").0;
        let spec = row.get::<Json<RunSpec>, _>("spec").0;
        let candidate = RunId::new(row.get::<String, _>("run_id"));
        if !conversational(&content)
            || spec.profile != head.profile
            || spec.permissions != head.permissions
            || spec.mode != head.mode
            || spec.limits != head.limits
            || spec.catalog_revision != head.catalog_revision
            || spec.policy_revision != head.policy_revision
            || !crate::steering::compatible_steering_budget_in(
                transaction,
                tenant,
                &candidate,
                target,
            )
            .await?
        {
            break;
        }
        sqlx::query("UPDATE cloud_session_submissions SET batch_run_id=$3, placement='steering', updated_at_ms=CASE WHEN updated_at_ms >= $4 THEN updated_at_ms + 1 ELSE $4 END
            WHERE tenant_id=$1 AND run_id=$2 AND placement='queued'")
            .bind(tenant.as_str()).bind(candidate.as_str()).bind(target.as_str()).bind(now)
            .execute(&mut **transaction).await.map_err(database_error)?;
    }
    Ok(())
}

impl CloudStore {
    pub async fn run_batch_for_worker(
        &self,
        worker_id: &str,
        run: &StartedRun,
        now_ms: u64,
    ) -> Result<Vec<SteeringInput>, HarnessError> {
        let mut transaction = self.tenant_transaction(&run.claim.tenant_id).await?;
        crate::store::require_writer_in(&mut transaction, run, worker_id, Some(now_ms)).await?;
        let rows = sqlx::query(
            "SELECT submission_id, run_id, input_provenance, content, submission_references,
            attachments, placement, created_at_ms, updated_at_ms FROM cloud_session_submissions
            WHERE tenant_id=$1 AND session_id=$2 AND batch_run_id=$3 ORDER BY fifo_position",
        )
        .bind(run.claim.tenant_id.as_str())
        .bind(run.claim.session_id.as_str())
        .bind(run.claim.run_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let inputs = rows
            .iter()
            .map(|row| {
                let submission = decode_submission(row)?;
                Ok(SteeringInput {
                    submission_id: submission.id.clone(),
                    input: submission.content.input().to_owned(),
                    display_input: None,
                    source: UserMessageSource::Submission {
                        submission_id: submission.id,
                        created_at_ms: submission.created_at_ms,
                        delivery: SubmissionDelivery::Queue,
                        regenerate_from: None,
                        skill_name: None,
                    },
                    provenance: submission.provenance,
                    references: submission.references,
                    reference_contexts: Vec::new(),
                    attachments: submission.attachments,
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        transaction.commit().await.map_err(database_error)?;
        Ok(inputs)
    }

    pub async fn resume_session_inbox(
        &self,
        tenant: &TenantId,
        actor: &UserId,
        session: &SessionId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.begin().await?;
        let owner = crate::sharing::session_owner_in(
            &mut transaction,
            tenant,
            actor,
            session,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        set_scope(&mut transaction, tenant, &owner).await?;
        require_owned_session(&mut transaction, tenant, &owner, session).await?;
        let now = to_i64(now_ms, "queue resume time")?;
        sqlx::query(
            "UPDATE cloud_session_inboxes SET paused=0, error=NULL, updated_at_ms=$3
            WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(tenant.as_str())
        .bind(session.as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        promote_head(&mut transaction, tenant, &owner, session, now).await?;
        transaction.commit().await.map_err(database_error)
    }
}
