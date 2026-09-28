use serde::{Deserialize, Serialize};

use crate::{ProviderModelDefaults, ProviderProtocol, UserId};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedModel {
    pub model_id: String,
    pub display_name: String,
    pub protocol: ProviderProtocol,
    pub defaults: ProviderModelDefaults,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelDeviceScope {
    Account {
        #[serde(default)]
        include_account_providers: bool,
    },
    Selected {
        grants: Vec<ModelDeviceGrantScope>,
        #[serde(default)]
        providers: Vec<ModelDeviceProviderScope>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDeviceProviderScope {
    pub provider_id: String,
    pub model_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDeviceProvider {
    pub provider_id: String,
    pub provider_name: String,
    pub models: Vec<PublishedModel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDeviceGrantScope {
    pub grant_id: String,
    pub model_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDeviceLimits {
    pub monthly_tokens: Option<u64>,
    pub max_concurrent_requests: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u32>,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDeviceIdentity {
    pub device_id: String,
    pub device_name: String,
    pub user_id: UserId,
    pub username: String,
    pub scope: ModelDeviceScope,
    #[serde(default)]
    pub limits: ModelDeviceLimits,
    pub revoked_at_ms: Option<u64>,
    pub created_at_ms: u64,
    pub last_used_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDeviceGrant {
    pub grant_id: String,
    pub grant_name: String,
    pub models: Vec<PublishedModel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDeviceSession {
    pub identity: ModelDeviceIdentity,
    pub grants: Vec<ModelDeviceGrant>,
    #[serde(default)]
    pub providers: Vec<ModelDeviceProvider>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDevicePage {
    pub devices: Vec<ModelDeviceIdentity>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ModelDevicePoll {
    Pending {
        interval: u64,
    },
    SlowDown {
        interval: u64,
    },
    Denied,
    Expired,
    Authorized {
        token: String,
        session: Box<ModelDeviceSession>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDeviceReview {
    pub device_name: String,
    pub user_code: String,
    pub expires_at_ms: u64,
    pub providers: Vec<ModelDeviceProvider>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_device_limits_preserve_optional_fields_and_reject_unknown_fields() {
        for value in [
            json!({}),
            json!({
                "monthly_tokens":null,"max_concurrent_requests":null,"expires_at_ms":null,
            }),
        ] {
            assert_eq!(
                serde_json::from_value::<ModelDeviceLimits>(value).unwrap(),
                ModelDeviceLimits::default()
            );
        }
        let limits = ModelDeviceLimits {
            monthly_tokens: Some(1000),
            max_concurrent_requests: Some(2),
            requests_per_minute: Some(30),
            expires_at_ms: Some(1_800_000_000_000),
        };
        assert_eq!(
            serde_json::from_value::<ModelDeviceLimits>(serde_json::to_value(&limits).unwrap())
                .unwrap(),
            limits
        );
        for value in [
            json!({"monthly_token":10}),
            json!({"monthly_tokens":-1}),
            json!({"max_concurrent_requests":4_294_967_296_u64}),
            json!({"expires_at_ms":"tomorrow"}),
        ] {
            assert!(serde_json::from_value::<ModelDeviceLimits>(value).is_err());
        }
    }

    #[test]
    fn model_device_identity_defaults_missing_limits_without_losing_explicit_limits() {
        let mut value = json!({
            "device_id":"mdv_test","device_name":"Laptop","user_id":"owner","username":"Owner",
            "scope":{"kind":"account"},"revoked_at_ms":null,"created_at_ms":1,"last_used_at_ms":null,
        });
        let identity: ModelDeviceIdentity = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(identity.limits, ModelDeviceLimits::default());
        value["limits"] =
            json!({"monthly_tokens":100,"max_concurrent_requests":2,"expires_at_ms":1000});
        let identity: ModelDeviceIdentity = serde_json::from_value(value).unwrap();
        let decoded: ModelDeviceIdentity =
            serde_json::from_value(serde_json::to_value(&identity).unwrap()).unwrap();
        assert_eq!(decoded.limits, identity.limits);
        assert_eq!(decoded.limits.monthly_tokens, Some(100));
    }
}
