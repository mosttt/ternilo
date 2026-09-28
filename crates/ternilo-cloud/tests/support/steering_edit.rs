use std::{collections::BTreeSet, time::Duration};

use sqlx::Row;
use ternilo_cloud::{
    CloudRunDraft, CloudSessionDraft, CloudSessionUpdate, CloudStore, CompiledRun, TerminalState,
};
use ternilo_control::{ControlStore, ControlUser, ResourceKind, ResourcePermissions};
use ternilo_protocol::{
    PermissionPreset, QueueEditRequest, RunId, RunModelBinding, RunModelSnapshot, SessionEvent,
    SessionEventKind, SessionId, SessionMode, SessionSubmissionRequest, SubmissionContent,
    SubmissionDelivery, UserMessageSource,
};
use ternilo_transport::{
    EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorHello, ExecutorId, ExecutorKind,
};

#[expect(
    clippy::too_many_lines,
    reason = "Keep delivery, edit recovery, contributor identity and transferred budget within one real shared session."
)]
pub async fn assert_edit_freeze(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    contributor: &ControlUser,
    template: &CompiledRun,
    now: u64,
) {
    let metadata = &template.spec.metadata;
    let tenant = &metadata.tenant_id;
    let session_id = SessionId::new("steering-edit-freeze");
    let snapshot =
        super::model_ledger::configure(control, owner, tenant, "steering-model", None, now).await;
    let session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: metadata.project_id.clone().unwrap(),
                workspace_id: metadata.workspace_id.clone(),
                session_id: Some(session_id.clone()),
                agent_id: metadata.agent_id.clone(),
                title: "Steering edit boundary".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: Some(snapshot.clone()),
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
            },
            tenant,
            &owner.user_id,
            now,
        )
        .await
        .unwrap();
    control
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
            &contributor.user_id,
            Some(ResourcePermissions {
                view: true,
                submit: true,
                ..ResourcePermissions::default()
            }),
            now,
        )
        .await
        .unwrap();
    let mut policy = super::policy();
    ternilo_cloud::CLOUD_CATALOG_REVISION.clone_into(&mut policy.catalog_revision);
    policy
        .allowed_plugin_kinds
        .insert(ternilo_cloud::BROKERED_MODEL_KIND.to_owned());
    let catalog = super::model_ledger::catalog(ternilo_cloud::CLOUD_CATALOG_REVISION);
    let build =
        |actor: &ControlUser, id: &str, input: &str, tokens: u64, model: &RunModelSnapshot| {
            policy
                .compile_run(
                    CloudRunDraft {
                        project_id: session.project_id.clone(),
                        workspace_id: session.workspace_id.clone(),
                        agent_id: session.agent_id.clone(),
                        session_id: session_id.clone(),
                        run_id: Some(RunId::new(id)),
                        limits: template.spec.limits,
                        permissions: session.permissions,
                        mode: session.mode,
                        profile: super::model_ledger::profile(model),
                        input: input.to_owned(),
                        references: Vec::new(),
                        reference_contexts: Vec::new(),
                        attachments: Vec::new(),
                        reserved_model_tokens: tokens,
                    },
                    tenant.clone(),
                    owner.user_id.clone(),
                    actor.user_id.clone(),
                    &catalog,
                )
                .unwrap()
        };
    let primary = build(
        owner,
        "steering-edit-primary",
        "Owner's running task",
        100,
        &snapshot,
    );
    cloud
        .enqueue_session_submission_as(&owner.user_id, &primary, &submission(&primary), now)
        .await
        .unwrap();
    let worker_id = "steering-edit-worker";
    super::worker_storage::bind_workers(cloud, &[worker_id], "shared-contract-storage").await;
    let capabilities = BTreeSet::from([
        ExecutorCapability::CloudRun,
        ExecutorCapability::AddressedSessionCommands,
        ExecutorCapability::SessionSteering,
    ]);
    let worker = cloud
        .register_cloud_worker(
            &ExecutorHello {
                protocol_version: EXECUTOR_PROTOCOL_VERSION,
                executor_id: ExecutorId::new(worker_id),
                executor_kind: ExecutorKind::CloudWorker,
                instance_nonce: "steering-edit-process".to_owned(),
                catalog_revision: ternilo_cloud::CLOUD_CATALOG_REVISION.to_owned(),
                capabilities: capabilities.clone(),
            },
            Duration::from_secs(120),
            now,
        )
        .await
        .unwrap();
    let claim = cloud
        .claim_run(worker_id, Duration::from_secs(120), now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.run_id, primary.spec.metadata.run_id);
    let running = cloud
        .start_run(claim, worker_id, Duration::from_secs(120), now)
        .await
        .unwrap()
        .unwrap();
    let candidate = build(
        contributor,
        "steering-edit-candidate",
        "Contributor's original payload",
        100,
        &snapshot,
    );
    let receipt = cloud
        .enqueue_session_submission_as(
            &contributor.user_id,
            &candidate,
            &submission(&candidate),
            now + 1,
        )
        .await
        .unwrap();
    let original_provenance = receipt.submission.provenance.clone().unwrap();
    assert_eq!(original_provenance.input_id, receipt.submission.id);
    assert_eq!(
        original_provenance.author,
        ternilo_protocol::InputAuthor::Account {
            user_id: contributor.user_id.clone(),
            username: contributor.username.clone()
        }
    );
    let ticket = cloud
        .begin_session_steering(
            tenant,
            &contributor.user_id,
            &session_id,
            &receipt.submission.id,
            now + 2,
        )
        .await
        .unwrap();
    let command_id = ticket.command_id.unwrap();
    let command = cloud
        .session_command_as(tenant, &owner.user_id, &session_id, &command_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command.actor_user_id, contributor.user_id);
    assert_eq!(
        command.contributor_user_id.as_ref(),
        Some(&contributor.user_id)
    );
    let mut altered_snapshot = snapshot.clone();
    if let RunModelBinding::UserProvider { provider_id, .. } = &mut altered_snapshot.binding {
        "other-provider".clone_into(provider_id);
    }
    let altered = build(
        owner,
        candidate.spec.metadata.run_id.as_str(),
        "Owner's replacement payload",
        100,
        &altered_snapshot,
    );
    let error = cloud
        .edit_queued_session_submission(
            tenant,
            &owner.user_id,
            &session_id,
            &receipt.submission.id,
            QueueEditRequest {
                input: altered.spec.input.clone(),
                expected_updated_at_ms: receipt.submission.updated_at_ms,
            },
            &altered,
            now + 3,
        )
        .await
        .unwrap_err();
    assert!(
        error.message.contains("being delivered"),
        "authorized replacements must fail because delivery freezes the payload: {error:?}"
    );
    let claimed = cloud
        .claim_session_commands(
            &worker,
            &capabilities,
            Duration::from_secs(30),
            100,
            now + 4,
        )
        .await
        .unwrap()
        .into_iter()
        .find(|claim| claim.command.command_id == command_id)
        .unwrap();
    let error = cloud
        .edit_queued_session_submission(
            tenant,
            &owner.user_id,
            &session_id,
            &receipt.submission.id,
            QueueEditRequest {
                input: altered.spec.input.clone(),
                expected_updated_at_ms: receipt.submission.updated_at_ms,
            },
            &altered,
            now + 5,
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("being delivered"));
    let delivered = cloud
        .steering_submission_for_worker(&worker, &claimed, now + 5)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.content.input(), candidate.spec.input);
    let mut tx = cloud
        .database()
        .owner_transaction(tenant, &owner.user_id)
        .await
        .unwrap();
    let row = sqlx::query("SELECT s.actor_user_id,r.spec FROM cloud_session_submissions s JOIN cloud_runs r ON r.tenant_id=s.tenant_id AND r.run_id=s.run_id WHERE s.tenant_id=$1 AND s.submission_id=$2")
        .bind(tenant.as_str()).bind(receipt.submission.id.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(
        row.get::<String, _>("actor_user_id"),
        contributor.user_id.as_str()
    );
    assert_eq!(
        row.get::<ternilo_storage::Json<ternilo_protocol::RunSpec>, _>("spec")
            .0,
        candidate.spec
    );
    tx.commit().await.unwrap();

    cloud
        .complete_steering_command(&worker, &claimed, false, now + 6)
        .await
        .unwrap();
    let replacement = build(
        owner,
        candidate.spec.metadata.run_id.as_str(),
        "Owner's edit after declined steering",
        100,
        &snapshot,
    );
    cloud
        .edit_queued_session_submission(
            tenant,
            &owner.user_id,
            &session_id,
            &receipt.submission.id,
            QueueEditRequest {
                input: replacement.spec.input.clone(),
                expected_updated_at_ms: cloud
                    .strict_steering_candidate(
                        tenant,
                        &owner.user_id,
                        &session_id,
                        &receipt.submission.id,
                    )
                    .await
                    .unwrap()
                    .updated_at_ms,
            },
            &replacement,
            now + 7,
        )
        .await
        .unwrap();
    let edited = cloud
        .session_inbox(tenant, &owner.user_id, &session_id)
        .await
        .unwrap();
    assert_eq!(
        edited
            .items
            .iter()
            .find(|item| item.id == receipt.submission.id)
            .unwrap()
            .provenance
            .as_ref(),
        Some(&original_provenance),
        "another editor must not replace the accepted author"
    );
    let next = cloud
        .begin_session_steering(
            tenant,
            &owner.user_id,
            &session_id,
            &receipt.submission.id,
            now + 8,
        )
        .await
        .unwrap();
    let next_command = next.command_id.unwrap();
    let record = cloud
        .session_command_as(tenant, &owner.user_id, &session_id, &next_command)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.contributor_user_id.as_ref(), Some(&owner.user_id));
    cloud
        .expire_pending_session_steering(
            tenant,
            &owner.user_id,
            &session_id,
            &receipt.submission.id,
            &next_command,
            now + 16_024,
        )
        .await
        .unwrap();
    let changed_default = cloud
        .update_session(
            tenant,
            &session_id,
            &owner.user_id,
            CloudSessionUpdate {
                reserved_model_tokens: Some(200),
                ..CloudSessionUpdate::default()
            },
            now + 16_025,
        )
        .await
        .unwrap();
    assert_eq!(changed_default.reserved_model_tokens, 200);
    assert_eq!(
        cloud.model_budget(&running, worker_id).await.unwrap(),
        (100, 0)
    );
    let queued = cloud
        .queued_submission_run(
            tenant,
            &contributor.user_id,
            &session_id,
            &receipt.submission.id,
        )
        .await
        .unwrap();
    assert_eq!(queued.reserved_model_tokens, 100);
    assert_eq!(queued.actor_user_id, owner.user_id);
    assert_eq!(queued.spec, replacement.spec);
    let after_timeout = build(
        contributor,
        candidate.spec.metadata.run_id.as_str(),
        "Contributor's edit after timeout",
        queued.reserved_model_tokens,
        &snapshot,
    );
    cloud
        .edit_queued_session_submission(
            tenant,
            &contributor.user_id,
            &session_id,
            &receipt.submission.id,
            QueueEditRequest {
                input: after_timeout.spec.input.clone(),
                expected_updated_at_ms: cloud
                    .strict_steering_candidate(
                        tenant,
                        &contributor.user_id,
                        &session_id,
                        &receipt.submission.id,
                    )
                    .await
                    .unwrap()
                    .updated_at_ms,
            },
            &after_timeout,
            now + 16_026,
        )
        .await
        .unwrap();
    let mut tx = cloud.database().tenant_transaction(tenant).await.unwrap();
    let ceiling: i64 = sqlx::query_scalar("SELECT q.reserved_model_tokens FROM cloud_runs r JOIN control_quota_reservations q ON q.tenant_id=r.tenant_id AND q.reservation_id=r.quota_reservation_id WHERE r.tenant_id=$1 AND r.run_id=$2")
        .bind(tenant.as_str()).bind(candidate.spec.metadata.run_id.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(
        ceiling, 100,
        "changing future defaults must not rewrite an already reserved submission"
    );
    tx.commit().await.unwrap();
    let final_ticket = cloud
        .begin_session_steering(
            tenant,
            &contributor.user_id,
            &session_id,
            &receipt.submission.id,
            now + 16_027,
        )
        .await
        .unwrap();
    let final_claim = cloud
        .claim_session_commands(
            &worker,
            &capabilities,
            Duration::from_secs(30),
            100,
            now + 16_028,
        )
        .await
        .unwrap()
        .into_iter()
        .find(|claim| Some(&claim.command.command_id) == final_ticket.command_id.as_ref())
        .unwrap();
    assert_eq!(
        cloud
            .steering_submission_for_worker(&worker, &final_claim, now + 16_028)
            .await
            .unwrap()
            .unwrap()
            .content
            .input(),
        after_timeout.spec.input
    );
    let delivered = cloud
        .steering_submission_for_worker(&worker, &final_claim, now + 16_028)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.provenance.as_ref(), Some(&original_provenance));
    cloud
        .complete_steering_command(&worker, &final_claim, true, now + 16_029)
        .await
        .unwrap();
    let steering_event = SessionEvent {
        seq: 0,
        occurred_at_ms: now + 16_030,
        run_id: running.claim.run_id.clone(),
        kind: SessionEventKind::UserMessage {
            provenance: Some(original_provenance.clone()),
            content: after_timeout.spec.input.clone(),
            display_content: None,
            source: Some(UserMessageSource::Submission {
                regenerate_from: None,
                submission_id: receipt.submission.id.clone(),
                created_at_ms: receipt.submission.created_at_ms,
                delivery: SubmissionDelivery::Steer,
                skill_name: None,
            }),
            references: Vec::new(),
            attachments: Vec::new(),
        },
    };
    let mut forged = steering_event.clone();
    if let SessionEventKind::UserMessage {
        provenance: Some(provenance),
        ..
    } = &mut forged.kind
    {
        provenance.author = ternilo_protocol::InputAuthor::Account {
            user_id: owner.user_id.clone(),
            username: owner.username.clone(),
        };
    }
    assert_eq!(
        cloud
            .append_event(&running, worker_id, &forged, now + 16_030)
            .await
            .unwrap_err()
            .code,
        ternilo_protocol::ErrorCode::PolicyDenied
    );
    assert_eq!(
        cloud.model_budget(&running, worker_id).await.unwrap(),
        (100, 0),
        "rejected authors cannot consume steering or transfer reservations"
    );
    cloud
        .append_event(&running, worker_id, &steering_event, now + 16_030)
        .await
        .unwrap();
    assert_eq!(
        cloud.model_budget(&running, worker_id).await.unwrap(),
        (200, 0),
        "steering transfers the candidate's actual old 100-token reservation"
    );
    assert_eq!(
        cloud
            .get_run(tenant, &primary.spec.metadata.run_id)
            .await
            .unwrap()
            .actor_user_id,
        owner.user_id
    );
    let persisted = cloud
        .session_command_as(
            tenant,
            &owner.user_id,
            &session_id,
            final_ticket.command_id.as_ref().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted.contributor_user_id.as_ref(),
        Some(&contributor.user_id)
    );
    let fresh = build(
        owner,
        "steering-edit-new-budget",
        "New task uses the changed default",
        changed_default.reserved_model_tokens,
        &snapshot,
    );
    let fresh_receipt = cloud
        .enqueue_session_submission_as(&owner.user_id, &fresh, &submission(&fresh), now + 16_031)
        .await
        .unwrap();
    let fresh_provenance = fresh_receipt.submission.provenance.as_ref().unwrap();
    assert_eq!(
        fresh_provenance.author,
        ternilo_protocol::InputAuthor::Account {
            user_id: owner.user_id.clone(),
            username: owner.username.clone()
        }
    );
    assert_ne!(fresh_provenance.input_id, original_provenance.input_id);
    let fresh_ticket = cloud
        .begin_session_steering(
            tenant,
            &owner.user_id,
            &session_id,
            &fresh_receipt.submission.id,
            now + 16_031,
        )
        .await
        .unwrap();
    let fresh_claim = cloud
        .claim_session_commands(
            &worker,
            &capabilities,
            Duration::from_secs(30),
            100,
            now + 16_031,
        )
        .await
        .unwrap()
        .into_iter()
        .find(|claim| Some(&claim.command.command_id) == fresh_ticket.command_id.as_ref())
        .unwrap();
    let fresh_input = cloud
        .steering_submission_for_worker(&worker, &fresh_claim, now + 16_031)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fresh_input.provenance.as_ref(),
        Some(fresh_provenance),
        "a new input by the editor has the editor's own author identity"
    );
    cloud
        .complete_steering_command(&worker, &fresh_claim, false, now + 16_031)
        .await
        .unwrap();
    let mut tx = cloud.database().tenant_transaction(tenant).await.unwrap();
    let fresh_ceiling: i64 = sqlx::query_scalar("SELECT reserved_model_tokens FROM control_quota_reservations WHERE tenant_id=$1 AND run_id=$2")
        .bind(tenant.as_str()).bind(fresh.spec.metadata.run_id.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(fresh_ceiling, 200);
    tx.commit().await.unwrap();
    cloud
        .cancel_run_as(
            tenant,
            &owner.user_id,
            &session_id,
            &fresh.spec.metadata.run_id,
            now + 16_032,
        )
        .await
        .unwrap();
    cloud
        .finish_run(
            &running,
            worker_id,
            TerminalState::Cancelled,
            None,
            None,
            now + 16_033,
        )
        .await
        .unwrap();
    cloud
        .release_resident(&(&running).into(), &worker, worker.generation, now + 16_033)
        .await
        .unwrap();
}

fn submission(compiled: &CompiledRun) -> SessionSubmissionRequest {
    SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: Some(compiled.spec.metadata.run_id.clone()),
        content: SubmissionContent::Prompt {
            input: compiled.spec.input.clone(),
        },
        references: Vec::new(),
        attachments: Vec::new(),
    }
}
