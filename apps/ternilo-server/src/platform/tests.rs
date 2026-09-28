use super::execution::{CloudRunWorkspaceResolution, cloud_run_workspace_resolution};
use super::*;
use crate::platform::http::bearer_token;
use salvo_core::{
    http::{HeaderValue, header},
    routing::PathState,
    test::TestClient,
};
use ternilo_protocol::ReasoningEffort;

#[tokio::test]
async fn control_api_router_covers_visible_platform_actions() {
    let cases = [
        TestClient::get("http://control.test/api/v1/live").build(),
        TestClient::get("http://control.test/api/v1/executors/connect").build(),
        TestClient::post("http://control.test/api/v1/enrollments/consume").build(),
        TestClient::get("http://control.test/api/v1/me").build(),
        TestClient::get("http://control.test/api/v1/cloud-config").build(),
        TestClient::get("http://control.test/api/v1/tenants").build(),
        TestClient::post("http://control.test/api/v1/tenants").build(),
        TestClient::get("http://control.test/api/v1/tenants/tenant/members").build(),
        TestClient::put("http://control.test/api/v1/tenants/tenant/members/user").build(),
        TestClient::delete("http://control.test/api/v1/tenants/tenant/members/user").build(),
        TestClient::get("http://control.test/api/v1/tenants/tenant/executors").build(),
        TestClient::delete("http://control.test/api/v1/tenants/tenant/executors/home").build(),
        TestClient::get("http://control.test/api/v1/tenants/tenant/my-computers").build(),
        TestClient::delete("http://control.test/api/v1/tenants/tenant/my-computers/home").build(),
        TestClient::post("http://control.test/api/v1/tenants/tenant/enrollments").build(),
        TestClient::post("http://control.test/api/v1/tenants/tenant/my-computer-enrollments")
            .build(),
        TestClient::get("http://control.test/api/v1/tenants/tenant/quota").build(),
        TestClient::put("http://control.test/api/v1/tenants/tenant/quota").build(),
        TestClient::get("http://control.test/api/v1/tenants/tenant/model-usage?period=2026-09")
            .build(),
        TestClient::get("http://control.test/api/v1/tenants/tenant/audit?limit=200").build(),
    ];
    for mut request in cases {
        let root = api_router();
        let mut path = PathState::from_owned_path(request.uri().path().to_owned());
        assert!(
            root.detect(&mut request, &mut path).await.is_some(),
            "{} {} must resolve to a Control platform handler",
            request.method(),
            request.uri().path(),
        );
    }
}

#[test]
fn cloud_model_snapshot_preserves_exact_reasoning_without_provider_secrets() {
    let snapshot = ternilo_protocol::RunModelSnapshot {
        binding: ternilo_protocol::RunModelBinding::Platform {
            grant_id: "grant".to_owned(),
            model_id: "reasoning-model".to_owned(),
            beneficiary_user_id: UserId::new("owner"),
        },
        protocol: ternilo_protocol::ProviderProtocol::OpenAiResponses,
        defaults: ternilo_protocol::ProviderModelDefaults {
            context_window: 200_000,
            max_output_tokens: 16_384,
            reasoning: Some(ternilo_protocol::ProviderModelReasoning {
                default_effort: ReasoningEffort::Low,
                efforts: [
                    (ReasoningEffort::Low, Some("minimal".to_owned())),
                    (ReasoningEffort::High, Some("ultra".to_owned())),
                ]
                .into_iter()
                .collect(),
            }),
        },
        reasoning_effort: Some(ReasoningEffort::High),
        display_name: "Reasoning model".to_owned(),
        source_name: "Owner allowance".to_owned(),
    };
    let profile = ternilo_cloud::cloud_profile(Some(&snapshot));
    let projected = ternilo_cloud::profile_model_snapshot(&profile)
        .unwrap()
        .unwrap();
    assert_eq!(projected, snapshot);
    assert_eq!(
        projected
            .resolved_model()
            .reasoning_value(projected.reasoning_effort)
            .unwrap(),
        Some("ultra")
    );
    let model = profile
        .plugins
        .iter()
        .find(|entry| entry.kind == ternilo_cloud::BROKERED_MODEL_KIND)
        .unwrap();
    for private in ["base_url", "api_key", "upstream_model"] {
        assert!(!model.config.to_string().contains(private));
    }
}

#[test]
fn bearer_parser_requires_exact_scheme_and_one_token() {
    let mut request = Request::new();
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer signed.jwt.value"),
    );
    assert_eq!(bearer_token(&request).unwrap(), "signed.jwt.value");

    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("bearer signed.jwt.value"),
    );
    assert!(bearer_token(&request).is_err());
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer one two"),
    );
    assert!(bearer_token(&request).is_err());
}

#[test]
fn cloud_run_workspace_resolution_preserves_only_existing_session_bindings() {
    let workspace = WorkspaceId::new("workspace-a");
    assert_eq!(
        cloud_run_workspace_resolution(None, &workspace, "project-a").unwrap(),
        CloudRunWorkspaceResolution::Registered,
    );
    assert_eq!(
        cloud_run_workspace_resolution(Some((&workspace, "project-a")), &workspace, "project-a",)
            .unwrap(),
        CloudRunWorkspaceResolution::SessionBinding,
    );
    assert!(
        cloud_run_workspace_resolution(
            Some((&WorkspaceId::new("workspace-b"), "project-a")),
            &workspace,
            "project-a",
        )
        .is_err()
    );
    assert!(
        cloud_run_workspace_resolution(Some((&workspace, "project-b")), &workspace, "project-a",)
            .is_err()
    );
}
