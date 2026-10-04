use std::{collections::BTreeMap, sync::Arc, time::Duration};

use salvo_core::prelude::{Depot, Json, Request, handler};
use serde::{Deserialize, Serialize};
use ternilo_control::{ControlStore, OidcAuthenticator, OidcConfig};
use ternilo_protocol::HarnessError;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use super::{ApiError, actor, app_state, invalid_request, now_ms, web::CloudWebAuth};

mod mail;
pub(super) use mail::AccountMailer;
use mail::MailSettings;
mod turnstile;
pub(super) use turnstile::TurnstileSettings;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoginSettings {
    pub public_url: String,
    #[serde(default)]
    pub oidc_providers: Vec<OidcSettings>,
    pub turnstile: Option<TurnstileSettings>,
    pub smtp: Option<MailSettings>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OidcSettings {
    pub id: String,
    pub name: String,
    pub enabled: bool,
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
    pub providers: BTreeMap<String, OidcProvider>,
    pub mailer: Option<Arc<AccountMailer>>,
    oidc_unavailable: bool,
    loaded_at: std::time::Instant,
}

pub(super) struct OidcProvider {
    pub name: String,
    pub auth: OidcAuthenticator,
    pub web: CloudWebAuth,
}

impl OidcProvider {
    async fn discover(
        oidc: &OidcSettings,
        public_url: &str,
        allow_insecure: bool,
    ) -> Result<Option<Self>, HarnessError> {
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
                    &oidc.id,
                    public_url,
                    &oidc.client_id,
                    &oidc.scopes,
                    allow_insecure,
                )?;
                Ok(Some(Self {
                    name: oidc.name.clone(),
                    auth,
                    web: web.with_client_secret(oidc.token_auth_method, oidc.client_secret.clone()),
                }))
            }
            Err(_) => Ok(None),
        }
    }
}

impl LoginRuntime {
    async fn build(
        settings: LoginSettings,
        revision: u64,
        allow_insecure: bool,
    ) -> Result<Self, HarnessError> {
        if let Some(turnstile) = &settings.turnstile {
            turnstile.validate()?;
        }
        let mailer = settings
            .smtp
            .as_ref()
            .map(|smtp| AccountMailer::new(smtp, &settings.public_url).map(Arc::new))
            .transpose()?;
        let mut runtime = Self {
            revision,
            settings,
            providers: BTreeMap::new(),
            mailer,
            oidc_unavailable: false,
            loaded_at: std::time::Instant::now(),
        };
        if (runtime
            .settings
            .oidc_providers
            .iter()
            .any(|provider| provider.enabled)
            || runtime.settings.turnstile.is_some())
            && super::web::validate_public_url(&runtime.settings.public_url, allow_insecure)
                .is_err()
        {
            // Keep native access available so the owner can correct the saved URL.
            // Configured Turnstile verification remains enforced by verify_turnstile.
            runtime.oidc_unavailable = runtime
                .settings
                .oidc_providers
                .iter()
                .any(|provider| provider.enabled);
            return Ok(runtime);
        }
        let mut identifiers = std::collections::BTreeSet::new();
        if runtime.settings.oidc_providers.len() > 16 {
            return Err(HarnessError::invalid(
                "at most 16 OIDC providers may be configured",
            ));
        }
        for oidc in &runtime.settings.oidc_providers {
            if oidc.id.is_empty()
                || oidc.id.len() > 64
                || !oidc
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                || !identifiers.insert(&oidc.id)
                || oidc.name.trim().is_empty()
                || oidc.name.len() > 100
                || oidc.name.chars().any(char::is_control)
            {
                return Err(HarnessError::invalid(
                    "OIDC providers need unique IDs and a display name",
                ));
            }
        }
        let discovered = futures_util::future::try_join_all(
            runtime
                .settings
                .oidc_providers
                .iter()
                .filter(|provider| provider.enabled)
                .map(|oidc| async {
                    Ok::<_, HarnessError>((
                        oidc.id.clone(),
                        OidcProvider::discover(oidc, &runtime.settings.public_url, allow_insecure)
                            .await?,
                    ))
                }),
        )
        .await?;
        for (id, provider) in discovered {
            if let Some(provider) = provider {
                runtime.providers.insert(id, provider);
            } else {
                runtime.oidc_unavailable = true;
            }
        }
        Ok(runtime)
    }

    pub(super) fn provider(&self, id: &str) -> Result<&OidcProvider, HarnessError> {
        self.providers.get(id).ok_or_else(|| {
            HarnessError::policy("selected OIDC provider is disabled or unavailable")
        })
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
    pub(super) trusted_proxy_ips: Vec<std::net::IpAddr>,
    fallback: LoginSettings,
    allow_insecure: bool,
    cache: Mutex<Option<Arc<LoginRuntime>>>,
}

impl SecurityState {
    pub(super) fn from_config(config: &crate::config::ServerConfig) -> Self {
        Self {
            trusted_proxy_ips: config.trusted_proxy_ips.clone(),
            fallback: LoginSettings {
                public_url: config.public_url.clone().unwrap_or_default(),
                oidc_providers: config
                    .oidc
                    .as_ref()
                    .map(|oidc| OidcSettings {
                        id: "organization".into(),
                        name: "Organization".into(),
                        enabled: true,
                        issuer: oidc.issuer.clone(),
                        audience: oidc.audience.clone(),
                        client_id: oidc.client_id.clone(),
                        scopes: oidc.scopes.clone(),
                        token_auth_method: TokenAuthMethod::None,
                        client_secret: None,
                    })
                    .into_iter()
                    .collect(),
                turnstile: None,
                smtp: None,
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
    oidc_providers: Vec<OidcSettings>,
    turnstile: Option<TurnstileSettings>,
    smtp: Option<MailSettings>,
}

fn public_settings(runtime: &LoginRuntime) -> serde_json::Value {
    let mut settings =
        serde_json::to_value(&runtime.settings).expect("login settings contain JSON values");
    settings["revision"] = runtime.revision.into();
    settings["oidc_unavailable"] = runtime.oidc_unavailable.into();
    for provider in settings["oidc_providers"]
        .as_array_mut()
        .expect("provider array")
    {
        let oidc = provider.as_object_mut().expect("provider object");
        let has_secret = oidc
            .remove("client_secret")
            .is_some_and(|value| value.as_str().is_some_and(|value| !value.is_empty()));
        oidc.insert("has_client_secret".into(), has_secret.into());
        oidc.insert(
            "available".into(),
            runtime
                .providers
                .contains_key(oidc["id"].as_str().expect("provider ID"))
                .into(),
        );
    }
    if let Some(turnstile) = settings["turnstile"].as_object_mut() {
        let has_secret = turnstile
            .remove("secret_key")
            .is_some_and(|value| value.as_str().is_some_and(|value| !value.is_empty()));
        turnstile.insert("has_secret_key".into(), has_secret.into());
    }
    if let Some(smtp) = settings["smtp"].as_object_mut() {
        let has_password = smtp
            .remove("password")
            .is_some_and(|value| value.as_str().is_some_and(|value| !value.is_empty()));
        smtp.insert("has_password".into(), has_password.into());
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
    for oidc in &mut body.oidc_providers {
        if oidc.token_auth_method == TokenAuthMethod::None {
            oidc.client_secret = None;
        } else if oidc.client_secret.as_ref().is_none_or(String::is_empty) {
            let previous = previous.settings.oidc_providers.iter().find(|old| {
                old.id == oidc.id && old.issuer == oidc.issuer && old.client_id == oidc.client_id
            });
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
    if let Some(smtp) = body.smtp.as_mut() {
        if smtp.username.as_ref().is_none_or(String::is_empty) {
            smtp.password = None;
        } else if smtp.password.as_ref().is_none_or(String::is_empty) {
            smtp.password = previous
                .settings
                .smtp
                .as_ref()
                .filter(|old| {
                    old.host == smtp.host
                        && old.port == smtp.port
                        && old.security == smtp.security
                        && old.username == smtp.username
                })
                .and_then(|old| old.password.clone());
        }
    }
    let settings = LoginSettings {
        public_url: body.public_url.trim_end_matches('/').to_owned(),
        oidc_providers: body.oidc_providers,
        turnstile: body.turnstile,
        smtp: body.smtp,
    };
    if settings
        .oidc_providers
        .iter()
        .any(|provider| provider.enabled)
        || settings.turnstile.is_some()
    {
        super::web::validate_public_url(&settings.public_url, state.security.allow_insecure)?;
    }
    let mut runtime =
        LoginRuntime::build(settings, body.revision, state.security.allow_insecure).await?;
    if state.store.registration_settings().await?.oidc_only && runtime.providers.is_empty() {
        return Err(HarnessError::invalid(
            "OAuth2-only registration requires at least one available OIDC provider",
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
