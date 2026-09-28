use ternilo_control::ResourcePermissions;
use ternilo_protocol::{
    AgentPresetCopyRequest, AgentPresetDocument, AgentPresetUpdateRequest, Profile,
};

use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify editor inheritance, saved overlays and shared-read authorization in one HTTP scenario."
)]
async fn preset_editor_get_has_real_base_without_persisting_it_or_expanding_sharing() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let session = fixture.session("preset-editor").await;
    let owner = &fixture.owner.session.user;
    let copied = fixture
        .state
        .store
        .copy_user_agent_preset(
            owner,
            &fixture.tenant,
            AgentPresetCopyRequest {
                from: "standard".to_owned(),
                id: "editor-copy".to_owned(),
                display_name: None,
            },
            fixture.now,
        )
        .await
        .unwrap();
    assert!(copied.base_profile.is_none());
    let base = ternilo_cloud::cloud_profile(None);
    let mut override_row = base
        .plugins
        .iter()
        .find(|plugin| plugin.id == "agent-loop")
        .unwrap()
        .clone();
    override_row.config["max_tool_calls"] = json!(37);
    fixture
        .state
        .cloud
        .update_session(
            &fixture.tenant,
            &session.session_id,
            &owner.user_id,
            ternilo_cloud::CloudSessionUpdate {
                profile_plugins: Some(vec![override_row.clone()]),
                ..Default::default()
            },
            fixture.now,
        )
        .await
        .unwrap();
    let path = format!(
        "/agent-presets/editor-copy?session_id={}",
        session.session_id
    );
    let mut response = fixture.get(&path, &fixture.owner, &fixture.tenant).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let view = response.take_json::<AgentPresetDocument>().await.unwrap();
    assert_eq!(view.profile, copied.profile);
    assert_eq!(
        view.base_profile,
        Some(base.clone()),
        "the current session's 37-call override is not editor inheritance"
    );
    let saved = fixture
        .state
        .store
        .update_user_agent_preset(
            owner,
            &fixture.tenant,
            "editor-copy",
            AgentPresetUpdateRequest {
                display_name: "Edited copy".to_owned(),
                description: String::new(),
                profile: Profile {
                    plugins: vec![override_row],
                },
            },
            fixture.now,
        )
        .await
        .unwrap();
    assert!(saved.base_profile.is_none());
    let raw = fixture
        .state
        .store
        .user_agent_preset(owner, &fixture.tenant, "editor-copy")
        .await
        .unwrap();
    assert!(raw.base_profile.is_none());
    assert_eq!(raw.profile.plugins.len(), 1);
    let mut reopened = fixture.get(&path, &fixture.owner, &fixture.tenant).await;
    let reopened = reopened.take_json::<AgentPresetDocument>().await.unwrap();
    assert_eq!(reopened.profile, raw.profile);
    assert_eq!(reopened.base_profile, Some(base));
    let collaborator = fixture.collaborator().await;
    fixture
        .state
        .store
        .set_resource_share(
            owner,
            &fixture.tenant,
            ResourceKind::Session,
            session.session_id.as_str(),
            &collaborator.session.user.user_id,
            Some(ResourcePermissions::OWNER),
            fixture.now,
        )
        .await
        .unwrap();
    let list = format!("/agent-presets?session_id={}", session.session_id);
    let mut roster = fixture.get(&list, &collaborator, &fixture.tenant).await;
    assert_eq!(roster.status_code, Some(StatusCode::OK));
    assert_eq!(
        roster.take_json::<Value>().await.unwrap()["authorable"],
        false
    );
    let denied = fixture.get(&path, &collaborator, &fixture.tenant).await;
    assert_eq!(
        denied.status_code,
        Some(StatusCode::FORBIDDEN),
        "a shared preset roster is not authority to read the owner's execution configuration"
    );
}
