use super::*;

pub(super) struct Fixture {
    pub store: ControlStore,
    pub admin: ControlUser,
    pub owner: ControlUser,
    pub other: ControlUser,
    pub grants: [String; 2],
}

#[derive(Clone, Copy)]
pub(super) enum Source {
    First,
    Second,
    Account,
}

impl Fixture {
    pub async fn new(store: ControlStore) -> Self {
        let (admin, _) = setup(&store).await;
        store
            .set_instance_mode(&admin, crate::InstanceMode::MultiUser, 1, NOW)
            .await
            .unwrap();
        let owner = user(&store, "limits-owner").await;
        let other = user(&store, "limits-other").await;
        let first = grant(&store, &admin, &owner, "First").await;
        let second = grant(&store, &admin, &owner, "Second").await;
        let tenant = store.account_provider_space(&owner.user_id).await.unwrap();
        let provider = super::super::account::profile();
        store
            .put_user_credential(&owner, &tenant, "PRIVATE_KEY", "private-secret", NOW)
            .await
            .unwrap();
        store
            .upsert_user_provider_profile(&owner, &tenant, provider, NOW)
            .await
            .unwrap();
        Self {
            store,
            admin,
            owner,
            other,
            grants: [first, second],
        }
    }

    pub async fn connect(
        &self,
        limits: &ModelDeviceLimits,
        now: u64,
    ) -> (String, ModelDeviceIdentity) {
        let authorization = self
            .store
            .begin_model_device_authorization("Limited laptop", now)
            .await
            .unwrap();
        self.store
            .decide_model_device_authorization(
                &self.owner,
                &authorization.user_code,
                Some(&ModelDeviceScope::Account {
                    include_account_providers: true,
                }),
                limits,
                now + 1,
            )
            .await
            .unwrap();
        let ModelDevicePoll::Authorized { token, session } = self
            .store
            .poll_model_device_authorization(&authorization.device_code, now + 5000)
            .await
            .unwrap()
        else {
            panic!("device did not connect")
        };
        assert_eq!(session.identity.limits, *limits);
        assert_eq!(session.identity.user_id, self.owner.user_id);
        (token, session.identity)
    }

    pub async fn reserve(
        &self,
        token: &str,
        source: Source,
        request: &ModelRequestInput,
        now: u64,
    ) -> Result<ModelRequestPermit, ModelAccessError> {
        match source {
            Source::First => {
                self.store
                    .reserve_device_model_request(token, &self.grants[0], request, now)
                    .await
            }
            Source::Second => {
                self.store
                    .reserve_device_model_request(token, &self.grants[1], request, now)
                    .await
            }
            Source::Account => {
                self.store
                    .reserve_device_account_request(token, "device-upstream", request, now)
                    .await
            }
        }
    }

    pub async fn usage(&self, id: &str, now: u64) -> (u64, u64, u64) {
        let usage = self
            .store
            .model_device_usage(&self.owner, id, now)
            .await
            .unwrap();
        (
            usage.used_tokens,
            usage.reserved_tokens,
            usage.active_requests,
        )
    }

    pub async fn settle(&self, request: &str, tokens: Option<u64>, now: u64) {
        self.store
            .settle_model_request(request, &settlement(tokens), now)
            .await
            .unwrap();
    }
}

async fn user(store: &ControlStore, name: &str) -> ControlUser {
    store
        .upsert_user(
            &crate::OidcPrincipal {
                issuer: "https://limits.example".to_owned(),
                subject: name.to_owned(),
                email: None,
                display_name: None,
            },
            name,
            NOW,
        )
        .await
        .unwrap()
}

async fn grant(
    store: &ControlStore,
    admin: &ControlUser,
    owner: &ControlUser,
    name: &str,
) -> String {
    store
        .save_model_grant(
            admin,
            None,
            &ModelGrantInput {
                name: name.to_owned(),
                subject: ModelGrantSubject::User {
                    id: owner.user_id.to_string(),
                },
                model_ids: vec!["model".to_owned()],
                monthly_tokens: 10_000,
                max_concurrent_requests: 2,
                expires_at_ms: None,
                allow_resource_sharing: false,
            },
            NOW,
        )
        .await
        .unwrap()
        .grant_id
}

pub(super) fn input(key: &str, tokens: u64) -> ModelRequestInput {
    ModelRequestInput {
        request_key: key.to_owned(),
        payload_hash: "c".repeat(64),
        model_id: "model".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        reserved_tokens: tokens,
    }
}

pub(super) fn settlement(tokens: Option<u64>) -> ModelRequestSettlement {
    ModelRequestSettlement {
        state: ModelRequestState::Cancelled,
        usage: tokens.map(|tokens| ServiceModelUsage {
            input_tokens: Some(tokens),
            output_tokens: Some(0),
            ..Default::default()
        }),
        upstream_request_id: None,
        error_code: Some("client_cancelled".to_owned()),
    }
}
