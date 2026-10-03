use super::*;

#[expect(
    clippy::too_many_lines,
    reason = "Verify the complete attribution and lifecycle contract across authorization and settlement."
)]
pub(super) async fn contract(
    store: &ControlStore,
    owner: &ControlUser,
    actor: &ControlUser,
    workload: &WorkloadModelPrincipal,
) {
    let (principal, mut grant) = prepare(store, owner, actor, workload).await;
    let RunModelBinding::Platform { grant_id, .. } = &principal.snapshot.binding else {
        unreachable!()
    };
    let baseline = store
        .account_model_traffic(actor, &actor.user_id, NOW)
        .await
        .unwrap();
    store
        .update_account_model_traffic(
            owner,
            &actor.user_id,
            baseline.revision,
            Some(&crate::ModelTrafficLimits {
                requests_per_minute: Some(u32::try_from(baseline.recent_requests + 1).unwrap()),
                max_concurrent_requests: None,
            }),
            NOW,
        )
        .await
        .unwrap();
    let permit = reserve_node(store, &principal, "first").await.unwrap();
    assert_eq!(permit.request.source, ModelRequestSource::PlatformGrant);
    assert_eq!(permit.request.grant_id.as_ref(), Some(grant_id));
    assert_eq!(permit.request.actor_user_id, actor.user_id);
    assert_eq!(permit.request.model_beneficiary_user_id, owner.user_id);
    assert!(
        !reserve_node(store, &principal, "first")
            .await
            .unwrap()
            .newly_accepted
    );
    assert!(reserve_node(store, &principal, "concurrent").await.is_err());
    let id = &permit.request.request_id;
    begin_node(store, &principal, id, 1).await.unwrap();
    store
        .settle_model_attempt(id, 1, &settlement(ModelRequestState::Failed, Some(60)), NOW)
        .await
        .unwrap();
    assert!(
        begin_node(store, &principal, id, 2).await.is_err(),
        "retry must reserve another attempt within the same grant"
    );
    grant.monthly_tokens = 300;
    store
        .save_model_grant(owner, Some(grant_id), &grant, NOW)
        .await
        .unwrap();
    begin_node(store, &principal, id, 2).await.unwrap();
    store
        .settle_model_attempt(
            id,
            2,
            &settlement(ModelRequestState::Completed, Some(20)),
            NOW,
        )
        .await
        .unwrap();
    store
        .finish_node_model_request(id, ModelRequestState::Completed, None, NOW)
        .await
        .unwrap();
    assert_eq!(
        store
            .get_model_grant(owner, grant_id, NOW)
            .await
            .unwrap()
            .quota
            .used_tokens,
        80
    );
    let counted = store
        .account_model_traffic(actor, &actor.user_id, NOW)
        .await
        .unwrap();
    assert_eq!(
        counted.recent_requests,
        baseline.recent_requests + 1,
        "two internal attempts consume one logical request"
    );
    assert_eq!(counted.active_requests, baseline.active_requests);
    store
        .update_account_model_traffic(owner, &actor.user_id, counted.revision, None, NOW)
        .await
        .unwrap();
    grant.allow_resource_sharing = false;
    store
        .save_model_grant(owner, Some(grant_id), &grant, NOW)
        .await
        .unwrap();
    assert!(
        reserve_node(store, &principal, "not-shareable")
            .await
            .is_err()
    );
    let own = NodeModelPrincipal {
        actor_user_id: owner.user_id.clone(),
        ..principal.clone()
    };
    let pending = reserve_node(store, &own, "pending-revocation")
        .await
        .unwrap();
    store
        .revoke_model_grant(owner, grant_id, NOW)
        .await
        .unwrap();
    let mut transaction = store.database().begin().await.unwrap();
    assert!(
        store
            .check_node_model_request_in(&mut transaction, &own, &pending.request.request_id, NOW)
            .await
            .is_err()
    );
    transaction.rollback().await.unwrap();
    assert!(reserve_node(store, &own, "revoked").await.is_err());
    store
        .finish_node_model_request(
            &pending.request.request_id,
            ModelRequestState::Cancelled,
            None,
            NOW,
        )
        .await
        .unwrap();
}

async fn prepare(
    store: &ControlStore,
    owner: &ControlUser,
    actor: &ControlUser,
    workload: &WorkloadModelPrincipal,
) -> (NodeModelPrincipal, ModelGrantInput) {
    let mut profile = store
        .get_model_provider(owner, "upstream")
        .await
        .unwrap()
        .profile;
    profile.id = "node-upstream".to_owned();
    store
        .save_model_provider(
            owner,
            &ModelProviderInput {
                profile,
                enabled: true,
                api_key: None,
                clear_api_key: false,
            },
            NOW,
        )
        .await
        .unwrap();
    store
        .save_model_publication(
            owner,
            &ModelPublicationInput {
                model_id: "node-public".to_owned(),
                display_name: "Node public".to_owned(),
                provider_id: "node-upstream".to_owned(),
                upstream_model: "upstream-model".to_owned(),
                enabled: true,
            },
            NOW,
        )
        .await
        .unwrap();
    let input = ModelGrantInput {
        name: "Node budget".to_owned(),
        subject: ModelGrantSubject::User {
            id: owner.user_id.to_string(),
        },
        model_ids: vec!["node-public".to_owned()],
        monthly_tokens: 150,
        max_concurrent_requests: 1,
        allow_resource_sharing: true,
        expires_at_ms: None,
    };
    let grant = store
        .save_model_grant(owner, None, &input, NOW)
        .await
        .unwrap();
    let binding = RunModelBinding::Platform {
        grant_id: grant.grant_id,
        model_id: "node-public".to_owned(),
        beneficiary_user_id: owner.user_id.clone(),
    };
    let snapshot = store
        .resolve_workload_model_snapshot(
            &actor.user_id,
            &owner.user_id,
            &workload.tenant_id,
            &binding,
            None,
            NOW,
        )
        .await
        .unwrap();
    (
        NodeModelPrincipal {
            credential_id: "node-test-credential".to_owned(),
            tenant_id: workload.tenant_id.clone(),
            session_id: SessionId::new("node-model-session"),
            run_id: RunId::new("node-model-run"),
            actor_user_id: actor.user_id.clone(),
            resource_owner_user_id: owner.user_id.clone(),
            snapshot,
        },
        input,
    )
}

async fn reserve_node(
    store: &ControlStore,
    principal: &NodeModelPrincipal,
    key: &str,
) -> Result<ModelRequestPermit, ModelAccessError> {
    let mut transaction = store.database().begin().await?;
    let result = store
        .reserve_node_model_request_in(
            &mut transaction,
            principal,
            &request(key, "node-public", 100),
            NOW,
        )
        .await?;
    transaction
        .commit()
        .await
        .map_err(ternilo_storage::database_error)?;
    Ok(result)
}

async fn begin_node(
    store: &ControlStore,
    principal: &NodeModelPrincipal,
    id: &str,
    attempt: u32,
) -> Result<ModelServiceAttempt, ModelAccessError> {
    let mut transaction = store.database().begin().await?;
    let result = store
        .begin_node_model_attempt_in(&mut transaction, principal, id, attempt, NOW)
        .await?;
    transaction
        .commit()
        .await
        .map_err(ternilo_storage::database_error)?;
    Ok(result)
}
