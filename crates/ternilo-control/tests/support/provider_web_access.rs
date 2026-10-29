use super::*;
use ternilo_protocol::{
    NodeRunContext, NodeWebAccessRequest, ProviderWebAccess, WebCapability, WebSearchMode,
};

pub(super) async fn contract(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    node: &NodePrincipal,
    body: &NodeModelRequest,
    snapshot: &ternilo_protocol::RunModelSnapshot,
) {
    let now = fixture.now + 500_000;
    let tenant = &fixture.tenant_a;
    let (original, mut provider) = configure_web_provider(store, fixture).await;
    let web = *provider.effective_web_access("private-model").unwrap();
    let mut request = NodeWebAccessRequest {
        session_id: body.session_id.clone(),
        binding: body.binding.clone(),
        invocation: Some(NodeRunContext::from(body)),
        credential: Some(WebCapability::Search),
        expected: Some(Box::new(web.clone())),
    };
    // Shared model use does not grant export of the owner's web key to someone else's computer.
    let denied = store
        .node_account_web_access(node, &request, now)
        .await
        .err()
        .unwrap();
    assert!(denied.message.contains("owner's computers"), "{denied}");
    request.credential = None;
    assert!(
        store
            .node_account_web_access(node, &request, now)
            .await
            .unwrap()
            .api_key
            .is_none()
    );

    provider.id = "own-web-provider".into();
    for (name, value) in [
        ("ACCOUNT_MODEL_KEY", "alice-model-secret"),
        ("WEB_ONLY_KEY", "alice-web-secret"),
    ] {
        store
            .put_user_credential(&fixture.alice, tenant, name, value, now)
            .await
            .unwrap();
    }
    store
        .upsert_user_provider_profile(&fixture.alice, tenant, provider, now)
        .await
        .unwrap();
    let binding = RunModelBinding::UserProvider {
        tenant_id: tenant.clone(),
        owner_user_id: fixture.alice.user_id.clone(),
        provider_id: "own-web-provider".into(),
        model: "private-model".into(),
    };
    let own = store
        .resolve_workload_model_snapshot(
            &fixture.alice.user_id,
            &fixture.alice.user_id,
            tenant,
            &binding,
            None,
            now,
        )
        .await
        .unwrap();
    store
        .set_edge_session_model_snapshot(
            &fixture.alice,
            tenant,
            &mapping.browser_session,
            Some(own),
        )
        .await
        .unwrap();
    request.binding = binding.clone();
    request.invocation.as_mut().unwrap().binding = binding;
    request.credential = Some(WebCapability::Search);
    let granted = store
        .node_account_web_access(node, &request, now)
        .await
        .unwrap();
    assert_eq!(granted.api_key.as_deref(), Some("alice-web-secret"));
    assert_ne!(granted.api_key.as_deref(), Some("alice-model-secret"));

    reject_retargeting_and_revocation(store, fixture, node, &request, now).await;
    // Restore the original model to keep this fixture's ledger and revocation checks independent.
    store
        .upsert_user_provider_profile(&fixture.bob, tenant, original, now + 3)
        .await
        .unwrap();
    store
        .set_edge_session_model_snapshot(
            &fixture.alice,
            tenant,
            &mapping.browser_session,
            Some(snapshot.clone()),
        )
        .await
        .unwrap();
}

async fn reject_retargeting_and_revocation(
    store: &ControlStore,
    fixture: &EdgeFixture,
    node: &NodePrincipal,
    request: &NodeWebAccessRequest,
    now: u64,
) {
    let tenant = &fixture.tenant_a;
    let mut forged = request.clone();
    forged.invocation = None;
    assert!(
        store
            .node_account_web_access(node, &forged, now)
            .await
            .is_err()
    );
    forged = request.clone();
    forged.invocation.as_mut().unwrap().origin_session_id = SessionId::new("unrelated-session");
    assert!(
        store
            .node_account_web_access(node, &forged, now)
            .await
            .is_err()
    );
    forged = request.clone();
    forged.expected.as_mut().unwrap().search.base_url =
        Some("https://credential-collector.example".into());
    assert!(
        store
            .node_account_web_access(node, &forged, now)
            .await
            .is_err()
    );
    forged = request.clone();
    if let RunModelBinding::UserProvider { provider_id, .. } = &mut forged.binding {
        *provider_id = "another-provider".into();
    }
    assert!(
        store
            .node_account_web_access(node, &forged, now)
            .await
            .is_err()
    );

    store
        .put_user_credential(
            &fixture.alice,
            tenant,
            "WEB_ONLY_KEY",
            "rotated-web-secret",
            now + 1,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .node_account_web_access(node, request, now + 1)
            .await
            .unwrap()
            .api_key
            .as_deref(),
        Some("rotated-web-secret")
    );
    store
        .delete_user_credential(&fixture.alice, tenant, "WEB_ONLY_KEY")
        .await
        .unwrap();
    assert!(
        store
            .node_account_web_access(node, request, now + 2)
            .await
            .is_err()
    );
}

async fn configure_web_provider(
    store: &ControlStore,
    fixture: &EdgeFixture,
) -> (ProviderProfile, ProviderProfile) {
    let tenant = &fixture.tenant_a;
    let now = fixture.now + 500_000;
    let original = store
        .user_provider_profile(&fixture.bob, tenant, "account-provider")
        .await
        .unwrap()
        .unwrap();
    let mut provider = original.clone();
    let mut web = ProviderWebAccess::default();
    web.search.mode = WebSearchMode::Deepseek;
    web.search.api_key_ref = Some("WEB_ONLY_KEY".into());
    let mut defaults = ProviderWebAccess::default();
    defaults.search.mode = WebSearchMode::Disabled;
    provider.web_access = Some(Box::new(defaults));
    provider
        .models
        .iter_mut()
        .find(|model| model.id == "private-model")
        .unwrap()
        .web_access = Some(Box::new(ternilo_protocol::ProviderModelWebAccess {
        search: Some(web.search.clone()),
        fetch: None,
    }));
    store
        .put_user_credential(&fixture.bob, tenant, "WEB_ONLY_KEY", "bob-web-secret", now)
        .await
        .unwrap();
    store
        .upsert_user_provider_profile(&fixture.bob, tenant, provider.clone(), now)
        .await
        .unwrap();
    (original, provider)
}
