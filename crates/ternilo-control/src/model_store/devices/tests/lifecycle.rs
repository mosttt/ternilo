use super::*;

pub(super) async fn account_revocation_contract(store: &ControlStore, owner: &ControlUser) {
    store
        .set_instance_mode(owner, crate::InstanceMode::MultiUser, 1, NOW + 700_000)
        .await
        .unwrap();
    let member = store
        .upsert_user(
            &crate::OidcPrincipal {
                issuer: "https://device.example".to_owned(),
                subject: "member".to_owned(),
                email: Some("member@device.example".to_owned()),
                display_name: None,
            },
            "device-member",
            NOW + 700_001,
        )
        .await
        .unwrap();
    let _grant = store
        .save_model_grant(
            owner,
            None,
            &ModelGrantInput {
                name: "Member allowance".to_owned(),
                subject: ModelGrantSubject::User {
                    id: member.user_id.as_str().to_owned(),
                },
                model_ids: vec!["model".to_owned()],
                monthly_tokens: 10_000,
                max_concurrent_requests: 1,
                expires_at_ms: None,
                allow_resource_sharing: false,
            },
            NOW + 700_002,
        )
        .await
        .unwrap();
    let pending = store
        .begin_model_device_authorization("Member laptop", NOW + 700_003)
        .await
        .unwrap();
    store
        .decide_model_device_authorization(
            &member,
            &pending.user_code,
            Some(&ModelDeviceScope::Account {
                include_account_providers: false,
            }),
            &ModelDeviceLimits::default(),
            NOW + 700_004,
        )
        .await
        .unwrap();
    let status = store.get_account(owner, &member.user_id).await.unwrap();
    let banned = store
        .set_account_status(
            owner,
            &member.user_id,
            crate::AccountStatusAction::Ban,
            status.status_revision,
            NOW + 700_005,
        )
        .await
        .unwrap();
    store
        .set_account_status(
            owner,
            &member.user_id,
            crate::AccountStatusAction::Unban,
            banned.status_revision,
            NOW + 700_006,
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .poll_model_device_authorization(&pending.device_code, NOW + 710_000)
            .await
            .unwrap(),
        ModelDevicePoll::Denied
    ));
    assert!(
        store
            .list_model_devices(&member, &PageQuery::default())
            .await
            .unwrap()
            .devices
            .is_empty()
    );
}
