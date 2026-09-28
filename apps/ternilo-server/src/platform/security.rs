use std::{sync::Arc, time::Duration};

use salvo_core::prelude::{Depot, Json, Request, handler};
use serde::{Deserialize, Serialize};
use ternilo_control::{ControlStore, OidcAuthenticator, OidcConfig};
use ternilo_protocol::HarnessError;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use super::{ApiError, actor, app_state, invalid_request, now_ms, web::CloudWebAuth};

mod turnstile;
pub(super) use turnstile::TurnstileSettings;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoginSettings {
    pub public_url: String,
    pub oidc: Option<OidcSettings>,
    pub turnstile: Option<TurnstileSettings>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OidcSettings {
    pub issuer: String,
    #[serde(default)]
    pub audience: String,
    pub client_id: String,
    pub scopes: String,
    #[serde(default)]
    pub token_auth_method: TokenAuthMethod,
    #[serde(default)]
    pub client_secret: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum TokenAuthMethod {
    #[default]
    None,
    ClientSecretBasic,
    ClientSecretPost,
}

pub(super) struct LoginRuntime {
    pub revision: u64,
    pub settings: LoginSettings,
    pub auth: Option<Arc<OidcAuthenticator>>,
    pub web_auth: Option<Arc<CloudWebAuth>>,
    oidc_unavailable: bool,
    loaded_at: std::time::Instant,
}

impl LoginRuntime {
    async fn build(
        settings: LoginSettings,
        revision: u64,
        allow_insecure: bool,
    ) -> Result<Self, HarnessError> {
        if settings.oidc.is_some() || settings.turnstile.is_some() {
            super::web::validate_public_url(&settings.public_url, allow_insecure)?;
        }
        if let Some(turnstile) = &settings.turnstile {
            turnstile.validate()?;
        }
        let mut runtime = Self {
            revision,
            settings,
            auth: None,
            web_auth: None,
            oidc_unavailable: false,
            loaded_at: std::time::Instant::now(),
        };
        if let Some(oidc) = &runtime.settings.oidc {
            if oidc.token_auth_method != TokenAuthMethod::None
                && oidc
                    .client_secret
                    .as_ref()
                    .is_none_or(|value| value.trim().is_empty())
            {
                return Err(HarnessError::invalid(
                    "OAuth client secret is required for the selected token authentication method",
                ));
            }
            if oidc
                .client_secret
                .as_ref()
                .is_some_and(|value| value.len() > 4096 || value.chars().any(char::is_control))
            {
                return Err(HarnessError::invalid("OAuth client secret is invalid"));
            }
            let authenticator = OidcAuthenticator::discover(OidcConfig {
                issuer: oidc.issuer.clone(),
                audience: oidc.audience.clone(),
                allow_insecure_discovery: allow_insecure,
                jwks_cache_ttl: Duration::from_secs(300),
            })
            .await;
            match authenticator {
                Ok(auth) => {
                    let web = CloudWebAuth::new(
                        &auth,
                        &runtime.settings.public_url,
                        &oidc.client_id,
                        &oidc.scopes,
                        allow_insecure,
                    )?;
                    runtime.auth = Some(Arc::new(auth));
                    runtime.web_auth = Some(Arc::new(
                        web.with_client_secret(oidc.token_auth_method, oidc.client_secret.clone()),
                    ));
                }
                Err(_) => runtime.oidc_unavailable = true,
            }
        }
        Ok(runtime)
    }

    pub(super) async fn verify_turnstile(
        &self,
        token: Option<&str>,
        action: &str,
    ) -> Result<(), HarnessError> {
        if let Some(settings) = &self.settings.turnstile {
            settings
                .verify(token, action, &self.settings.public_url)
                .await?;
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct SecurityState {
    fallback: LoginSettings,
    allow_insecure: bool,
    cache: Mutex<Option<Arc<LoginRuntime>>>,
}

impl SecurityState {
    pub(super) fn from_config(config: &crate::config::ServerConfig) -> Self {
        Self {
            fallback: LoginSettings {
                public_url: config.public_url.clone().unwrap_or_default(),
                oidc: config.oidc.as_ref().map(|oidc| OidcSettings {
                    issuer: oidc.issuer.clone(),
                    audience: oidc.audience.clone(),
                    client_id: oidc.client_id.clone(),
                    scopes: oidc.scopes.clone(),
                    token_auth_method: TokenAuthMethod::None,
                    client_secret: None,
                }),
                turnstile: None,
            },
            allow_insecure: config.oidc.as_ref().is_some_and(|oidc| oidc.allow_insecure),
            cache: Mutex::new(None),
        }
    }

    pub(super) async fn current(
        &self,
        store: &ControlStore,
    ) -> Result<Arc<LoginRuntime>, HarnessError> {
        let revision = store.authentication_settings_revision().await?;
        let mut cache = self.cache.lock().await;
        if let Some(runtime) = cache.as_ref().filter(|runtime| {
            runtime.revision == revision
                && (!runtime.oidc_unavailable
                    || runtime.loaded_at.elapsed() < Duration::from_secs(30))
        }) {
            return Ok(Arc::clone(runtime));
        }
        let (revision, settings) = match store.authentication_settings().await? {
            Some((revision, bytes)) => (
                revision,
                serde_json::from_slice(&bytes).map_err(|_| {
                    HarnessError::execution("stored authentication settings are invalid")
                })?,
            ),
            None => (0, self.fallback.clone()),
        };
        let runtime = Arc::new(LoginRuntime::build(settings, revision, self.allow_insecure).await?);
        *cache = Some(Arc::clone(&runtime));
        Ok(runtime)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateLoginSettings {
    revision: u64,
    public_url: String,
    oidc: Option<OidcSettings>,
    turnstile: Option<TurnstileSettings>,
}

fn public_settings(runtime: &LoginRuntime) -> serde_json::Value {
    let mut settings =
        serde_json::to_value(&runtime.settings).expect("login settings contain JSON values");
    settings["revision"] = runtime.revision.into();
    settings["oidc_unavailable"] = runtime.oidc_unavailable.into();
    if let Some(oidc) = settings["oidc"].as_object_mut() {
        let has_secret = oidc
            .remove("client_secret")
            .is_some_and(|value| value.as_str().is_some_and(|value| !value.is_empty()));
        oidc.insert("has_client_secret".into(), has_secret.into());
    }
    if let Some(turnstile) = settings["turnstile"].as_object_mut() {
        let has_secret = turnstile
            .remove("secret_key")
            .is_some_and(|value| value.as_str().is_some_and(|value| !value.is_empty()));
        turnstile.insert("has_secret_key".into(), has_secret.into());
    }
    settings
}

#[handler]
pub(super) async fn get_settings(depot: &mut Depot) -> Result<Json<serde_json::Value>, ApiError> {
    let state = app_state(depot);
    state
        .store
        .require_authentication_settings_owner(actor(depot))
        .await?;
    let runtime = state.security.current(&state.store).await?;
    Ok(Json(public_settings(&runtime)))
}

#[handler]
pub(super) async fn update_settings(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let state = app_state(depot);
    state
        .store
        .require_authentication_settings_owner(actor(depot))
        .await?;
    let mut body = request
        .parse_json::<UpdateLoginSettings>()
        .await
        .map_err(invalid_request)?;
    let previous = state.security.current(&state.store).await?;
    if previous.revision != body.revision {
        return Err(HarnessError::conflict(
            "authentication settings changed; reload before saving",
        )
        .into());
    }
    if let Some(oidc) = body.oidc.as_mut() {
        if oidc.token_auth_method == TokenAuthMethod::None {
            oidc.client_secret = None;
        } else if oidc.client_secret.as_ref().is_none_or(String::is_empty) {
            let previous = previous
                .settings
                .oidc
                .as_ref()
                .filter(|old| old.issuer == oidc.issuer && old.client_id == oidc.client_id);
            oidc.client_secret = previous.and_then(|old| old.client_secret.clone());
        }
    }
    if let Some(turnstile) = body.turnstile.as_mut()
        && turnstile.secret_key.as_ref().is_none_or(String::is_empty)
    {
        turnstile.secret_key = previous
            .settings
            .turnstile
            .as_ref()
            .filter(|old| old.site_key == turnstile.site_key)
            .and_then(|old| old.secret_key.clone());
    }
    let settings = LoginSettings {
        public_url: body.public_url.trim_end_matches('/').to_owned(),
        oidc: body.oidc,
        turnstile: body.turnstile,
    };
    let mut runtime =
        LoginRuntime::build(settings, body.revision, state.security.allow_insecure).await?;
    if runtime.oidc_unavailable {
        return Err(HarnessError::invalid(
            "OIDC discovery or signing keys could not be verified; existing settings were kept",
        )
        .into());
    }
    let bytes = Zeroizing::new(
        serde_json::to_vec(&runtime.settings)
            .map_err(|_| HarnessError::execution("encode authentication settings"))?,
    );
    runtime.revision = state
        .store
        .set_authentication_settings(actor(depot), body.revision, &bytes, now_ms()?)
        .await?;
    let response = public_settings(&runtime);
    *state.security.cache.lock().await = Some(Arc::new(runtime));
    Ok(Json(response))
}
