use ternilo_cloud::{CloudSessionDraft, CloudSessionRecord, CloudStore, CompiledRun};
use ternilo_control::{ControlStore, ControlUser, ResourceKind, ResourcePermissions};
use ternilo_protocol::{
    ErrorCode, QueueEditRequest, RunId, SessionId, SessionSubmission, SessionSubmissionRequest,
    SubmissionContent, SubmissionDelivery,
};

async fn prepare_shared_queue(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    member: &ControlUser,
    template: &CompiledRun,
    now: u64,
) -> (CloudSessionRecord, CompiledRun, SessionSubmission) {
    let tenant = &template.spec.metadata.tenant_id;
    let session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: template.spec.metadata.project_id.clone().unwrap(),
                workspace_id: template.spec.metadata.workspace_id.clone(),
                session_id: Some(SessionId::new("queue-edit-conflicts")),
                agent_id: template.spec.metadata.agent_id.clone(),
                title: "Queue edit conflicts".to_owned(),
                permissions: template.spec.permissions,
                model: None,
                reserved_model_tokens: template.reserved_model_tokens,
                agent_preset: "standard".to_owned(),
                profile_plugins: vec![],
                mode: template.spec.mode,
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
            session.session_id.as_str(),
            &member.user_id,
            Some(ResourcePermissions {
                view: true,
                submit: true,
                ..ResourcePermissions::default()
            }),
            now,
        )
        .await
        .unwrap();
    let mut queued = template.clone();
    queued.spec.metadata.session_id = session.session_id.clone();
    queued.authorization_session_id = session.session_id.clone();
    queued.actor_user_id = owner.user_id.clone();
    queued.spec.metadata.run_id = RunId::new("queue-edit-running");
    cloud
        .enqueue_session_submission_as(&owner.user_id, &queued, &submission(&queued), now)
        .await
        .unwrap();
    queued.actor_user_id = member.user_id.clone();
    queued.spec.metadata.run_id = RunId::new("queue-edit-pending");
    let accepted = cloud
        .enqueue_session_submission_as(&member.user_id, &queued, &submission(&queued), now)
        .await
        .unwrap()
        .submission;
    (session, queued, accepted)
}

pub async fn assert_shared_edit_conflicts(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    member: &ControlUser,
    template: &CompiledRun,
    now: u64,
) {
    let (session, queued, accepted) =
        prepare_shared_queue(control, cloud, owner, member, template, now).await;
    let tenant = &template.spec.metadata.tenant_id;
    let mut owner_edit = queued.clone();
    owner_edit.actor_user_id = owner.user_id.clone();
    "owner edit".clone_into(&mut owner_edit.spec.input);
    let mut member_edit = queued.clone();
    "member edit".clone_into(&mut member_edit.spec.input);
    let (owner_result, member_result) = tokio::join!(
        cloud.edit_queued_session_submission(
            tenant,
            &owner.user_id,
            &session.session_id,
            &accepted.id,
            edit(&owner_edit, accepted.updated_at_ms),
            &owner_edit,
            now
        ),
        cloud.edit_queued_session_submission(
            tenant,
            &member.user_id,
            &session.session_id,
            &accepted.id,
            edit(&member_edit, accepted.updated_at_ms),
            &member_edit,
            now
        ),
    );
    let (winner, loser, winning_spec) = match (owner_result, member_result) {
        (Ok(winner), Err(loser)) => (winner, loser, &owner_edit),
        (Err(loser), Ok(winner)) => (winner, loser, &member_edit),
        results => panic!("exactly one shared edit must win: {results:?}"),
    };
    assert_eq!(loser.code, ErrorCode::Conflict);
    assert_eq!(winner.updated_at_ms, now + 1);
    assert_eq!(winner.provenance, accepted.provenance);
    assert_eq!(winner.created_at_ms, accepted.created_at_ms);
    let stored = cloud
        .queued_submission_run(tenant, &owner.user_id, &session.session_id, &accepted.id)
        .await
        .unwrap();
    assert_eq!(stored.spec, winning_spec.spec);
    assert_eq!(stored.actor_user_id, winning_spec.actor_user_id);
    assert_stale_edit(control, cloud, owner, &member_edit, &accepted, &winner).await;
    let next = cloud
        .edit_queued_session_submission(
            tenant,
            &member.user_id,
            &session.session_id,
            &accepted.id,
            edit(&member_edit, winner.updated_at_ms),
            &member_edit,
            now - 1,
        )
        .await
        .unwrap();
    assert_eq!(next.updated_at_ms, winner.updated_at_ms + 1);
    assert_eq!(next.provenance, accepted.provenance);
    assert_revoked_editor(control, cloud, owner, member, &member_edit, &next).await;
    assert_deleted_edit(cloud, &owner_edit, &next).await;
}

async fn assert_stale_edit(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    compiled: &CompiledRun,
    accepted: &SessionSubmission,
    winner: &SessionSubmission,
) {
    let metadata = &compiled.spec.metadata;
    let tenant = &metadata.tenant_id;
    let session_id = &metadata.session_id;
    let audits = control.list_audit(owner, tenant, 100).await.unwrap();
    let original_run = cloud
        .queued_submission_run(tenant, &owner.user_id, session_id, &accepted.id)
        .await
        .unwrap();
    let mut stale = compiled.clone();
    "rejected stale replacement".clone_into(&mut stale.spec.input);
    assert_eq!(
        cloud
            .edit_queued_session_submission(
                tenant,
                &compiled.actor_user_id,
                session_id,
                &accepted.id,
                edit(&stale, accepted.updated_at_ms),
                &stale,
                winner.updated_at_ms + 100
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        control.list_audit(owner, tenant, 100).await.unwrap(),
        audits
    );
    assert_eq!(
        cloud
            .queued_submission_run(tenant, &owner.user_id, session_id, &accepted.id)
            .await
            .unwrap(),
        original_run
    );
    assert_eq!(
        cloud
            .strict_steering_candidate(tenant, &owner.user_id, session_id, &accepted.id)
            .await
            .unwrap(),
        *winner
    );
}

async fn assert_deleted_edit(
    cloud: &CloudStore,
    compiled: &CompiledRun,
    current: &SessionSubmission,
) {
    let metadata = &compiled.spec.metadata;
    let tenant = &metadata.tenant_id;
    cloud
        .remove_queued_session_submission(
            tenant,
            &compiled.actor_user_id,
            &metadata.session_id,
            &current.id,
            current.updated_at_ms + 101,
        )
        .await
        .unwrap();
    assert_eq!(
        cloud
            .edit_queued_session_submission(
                tenant,
                &compiled.actor_user_id,
                &metadata.session_id,
                &current.id,
                edit(compiled, current.updated_at_ms),
                compiled,
                current.updated_at_ms + 102
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

async fn assert_revoked_editor(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    member: &ControlUser,
    compiled: &CompiledRun,
    current: &SessionSubmission,
) {
    let metadata = &compiled.spec.metadata;
    let tenant = &metadata.tenant_id;
    let session = &metadata.session_id;
    for permissions in [
        Some(ResourcePermissions {
            view: true,
            ..ResourcePermissions::default()
        }),
        None,
    ] {
        control
            .set_resource_share(
                owner,
                tenant,
                ResourceKind::Session,
                session.as_str(),
                &member.user_id,
                permissions,
                current.updated_at_ms,
            )
            .await
            .unwrap();
        for revision in [current.updated_at_ms, current.updated_at_ms - 1] {
            let error = cloud
                .edit_queued_session_submission(
                    tenant,
                    &member.user_id,
                    session,
                    &current.id,
                    edit(compiled, revision),
                    compiled,
                    current.updated_at_ms,
                )
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::PolicyDenied);
        }
        assert_eq!(
            cloud
                .strict_steering_candidate(tenant, &owner.user_id, session, &current.id)
                .await
                .unwrap(),
            *current
        );
    }
}

fn edit(compiled: &CompiledRun, expected_updated_at_ms: u64) -> QueueEditRequest {
    QueueEditRequest {
        input: compiled.spec.input.clone(),
        expected_updated_at_ms,
    }
}

fn submission(compiled: &CompiledRun) -> SessionSubmissionRequest {
    SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: Some(compiled.spec.metadata.run_id.clone()),
        content: SubmissionContent::Prompt {
            input: compiled.spec.input.clone(),
        },
        references: compiled.spec.references.clone(),
        attachments: compiled.spec.attachments.clone(),
    }
}
