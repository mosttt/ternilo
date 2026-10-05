use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{net::IpAddr, time::Duration};

use salvo_core::{
    http::{HeaderValue, header},
    prelude::{Depot, FlowCtrl, Request, Response, Router, Text, handler},
};
use salvo_extra::size_limiter::max_size;
use serde::{Deserialize, Serialize};
use ternilo_control::OidcAuthenticator;
use ternilo_protocol::HarnessError;

use super::{ApiError, app_state, security::TokenAuthMethod};

pub(crate) const BASE_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; style-src-elem 'self' 'sha256-nzTgYzXYDNe6BAHiiI7NNlfK8n/auuOAhh2t92YvuXo=' 'sha256-441zG27rExd4/il+NvIqyL8zFx5XmyNQtE381kSkUJk=' 'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU='; style-src-attr 'unsafe-inline'; img-src 'self' data: blob:; frame-src 'self' blob:; connect-src 'self'; worker-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; manifest-src 'self'";

const MAX_TOKEN_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_AUTH_BODY_BYTES: u64 = 128 * 1024;

mod login;

pub(super) struct CloudWebAuth {
    authorization_endpoint: String,
    token_endpoint: String,
    client_id: String,
    redirect_uri: String,
    scope: String,
    client: reqwest::Client,
    token_auth_method: TokenAuthMethod,
    client_secret: Option<String>,
    pub(super) binding: String,
}

impl CloudWebAuth {
    pub(super) fn with_client_secret(
        mut self,
        method: TokenAuthMethod,
        secret: Option<String>,
    ) -> Self {
        self.token_auth_method = method;
        self.client_secret = secret;
        self
    }

    pub(super) fn public_config(&self) -> BrowserAuthConfig {
        BrowserAuthConfig {
            authorization_endpoint: self.authorization_endpoint.clone(),
            client_id: self.client_id.clone(),
            redirect_uri: self.redirect_uri.clone(),
            scope: self.scope.clone(),
        }
    }

    pub(super) fn new(
        authenticator: &OidcAuthenticator,
        provider_id: &str,
        public_url: &str,
        client_id: &str,
        scope: &str,
        allow_insecure_loopback: bool,
    ) -> Result<Self, HarnessError> {
        let public_url = validate_public_url(public_url, allow_insecure_loopback)?;
        if client_id.trim().is_empty()
            || client_id.len() > 512
            || client_id.chars().any(char::is_control)
        {
            return Err(HarnessError::invalid(
                "OIDC client id must contain 1 to 512 bytes without control characters",
            ));
        }
        let scopes = scope.split_ascii_whitespace().collect::<Vec<_>>();
        if scopes.is_empty()
            || !scopes.contains(&"openid")
            || scopes.iter().any(|item| {
                item.len() > 128
                    || !item.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                    })
            })
        {
            return Err(HarnessError::invalid(
                "OIDC scopes must include openid and use simple space-separated scope names",
            ));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                HarnessError::execution(format!("build OIDC token client: {error}"))
            })?;
        Ok(Self {
            binding: URL_SAFE_NO_PAD.encode(Sha256::digest(format!(
                "{provider_id}\n{}\n{client_id}",
                authenticator.issuer()
            ))),
            authorization_endpoint: authenticator.authorization_endpoint().to_owned(),
            token_endpoint: authenticator.token_endpoint().to_owned(),
            client_id: client_id.to_owned(),
            redirect_uri: format!("{public_url}/auth/callback"),
            scope: scopes.join(" "),
            client,
            token_auth_method: TokenAuthMethod::None,
            client_secret: None,
        })
    }
}

pub(super) fn router() -> Router {
    Router::new()
        .hoop(web_security_headers)
        .hoop(max_size(MAX_AUTH_BODY_BYTES))
        .push(crate::assets::router())
        .push(Router::with_path("admin").get(crate::assets::index))
        .push(Router::with_path("admin/accounts").get(crate::assets::index))
        .push(Router::with_path("admin/instance").get(crate::assets::index))
        .push(Router::with_path("admin/workers").get(crate::assets::index))
        .push(Router::with_path("admin/models").get(crate::assets::index))
        .push(Router::with_path("models").get(crate::assets::index))
        .push(Router::with_path("model-connect").get(crate::assets::index))
        .push(Router::with_path("files").get(crate::assets::index))
        .push(Router::with_path("settings").get(crate::assets::index))
        .push(Router::with_path("settings/{section}").get(crate::assets::index))
        .push(Router::with_path("spaces/current").get(crate::assets::index))
        .push(Router::with_path("auth/callback").get(crate::assets::index))
        .push(Router::with_path("auth/verify-email").get(crate::assets::index))
        .push(Router::with_path("auth/reset-password").get(crate::assets::index))
        .push(Router::with_path("auth/recover").get(crate::assets::index))
        .push(Router::with_path("auth/config").get(super::identity::auth_config))
        .push(Router::with_path("auth/token").post(login::exchange_code))
        .push(Router::with_path("auth/mfa").post(login::complete_mfa))
        .push(Router::with_path("auth/refresh").post(login::refresh_token))
        .push(Router::with_path("assets/boot.js").get(boot_script))
}

#[derive(Serialize)]
pub(super) struct BrowserAuthConfig {
    authorization_endpoint: String,
    client_id: String,
    redirect_uri: String,
    scope: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeExchangeRequest {
    provider_id: String,
    code: String,
    code_verifier: String,
    nonce: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshRequest {
    provider_id: String,
    refresh_token: String,
}

#[derive(Deserialize)]
struct ProviderTokenResponse {
    access_token: String,
    token_type: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    scope: Option<String>,
    id_token: Option<String>,
}

#[derive(Serialize)]
struct BrowserTokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
}

#[handler]
async fn web_security_headers(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
    control: &mut FlowCtrl,
) {
    control.call_next(request, depot, response).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(BASE_SECURITY_POLICY),
    );
    if app_state(depot)
        .security
        .current(&app_state(depot).store)
        .await
        .is_ok_and(|runtime| runtime.settings.turnstile.is_some())
    {
        let policy = headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap_or_default()
            .replace(
                "script-src 'self'",
                "script-src 'self' https://challenges.cloudflare.com",
            )
            .replace(
                "frame-src 'self' blob:",
                "frame-src 'self' blob: https://challenges.cloudflare.com",
            );
        if let Ok(value) = HeaderValue::from_str(&policy) {
            headers.insert(header::CONTENT_SECURITY_POLICY, value);
        }
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
}

async fn exchange(
    auth: &CloudWebAuth,
    form: &[(&str, &str)],
) -> Result<ProviderTokenResponse, ApiError> {
    let mut form = form.to_vec();
    if auth.token_auth_method == TokenAuthMethod::ClientSecretPost {
        form.push((
            "client_secret",
            auth.client_secret.as_deref().unwrap_or_default(),
        ));
    }
    let mut request = auth
        .client
        .post(&auth.token_endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form);
    if auth.token_auth_method == TokenAuthMethod::ClientSecretBasic {
        request = request.basic_auth(
            form_encode(&auth.client_id),
            Some(form_encode(
                auth.client_secret.as_deref().unwrap_or_default(),
            )),
        );
    }
    let mut response = request.send().await.map_err(|error| {
        ApiError::unavailable(HarnessError::execution(format!(
            "contact OIDC token endpoint: {error}"
        )))
    })?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| HarnessError::unavailable("read OIDC token response"))?
    {
        if bytes.len() + chunk.len() > MAX_TOKEN_RESPONSE_BYTES {
            return Err(HarnessError::unavailable("OIDC token response is too large").into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(ApiError::unauthorized(HarnessError::policy(
            "OIDC token exchange was rejected",
        )));
    }
    let token: ProviderTokenResponse = serde_json::from_slice(&bytes).map_err(|error| {
        ApiError::unavailable(HarnessError::execution(format!(
            "decode OIDC token response: {error}"
        )))
    })?;
    if !token.token_type.eq_ignore_ascii_case("bearer")
        || token.access_token.is_empty()
        || token.access_token.len() > 16 * 1024
        || token.access_token.chars().any(char::is_whitespace)
        || token
            .refresh_token
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 64 * 1024)
    {
        return Err(ApiError::unavailable(HarnessError::execution(
            "OIDC token endpoint returned an invalid bearer token",
        )));
    }
    Ok(token)
}

fn form_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn valid_pkce_verifier(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

pub(super) fn validate_public_url(
    value: &str,
    allow_insecure_loopback: bool,
) -> Result<String, HarnessError> {
    let parsed = reqwest::Url::parse(value)
        .map_err(|error| HarnessError::invalid(format!("parse public URL: {error}")))?;
    if parsed.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_unspecified())
    }) {
        return Err(HarnessError::invalid(
            "public URL must use a reachable hostname or IP address, not a wildcard listening address",
        ));
    }
    let loopback = parsed.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if parsed.scheme() != "https"
        && !(allow_insecure_loopback && parsed.scheme() == "http" && loopback)
    {
        return Err(HarnessError::policy(
            "public URL must use HTTPS; HTTP is limited to explicit loopback development",
        ));
    }
    if parsed.cannot_be_a_base()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err(HarnessError::invalid(
            "public URL must be an origin without credentials, path, query, or fragment",
        ));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

#[handler]
fn boot_script(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.render(Text::Js(
        "window.__TERNILO_BOOT__ = { apiToken: '', remote: true, platform: true, providerAuthoring: true };",
    ));
}

#[cfg(test)]
mod tests {
    use salvo_core::{routing::PathState, test::TestClient};

    use super::{router, valid_pkce_verifier, validate_public_url};

    #[tokio::test]
    async fn oidc_browser_routes_are_mounted_with_exact_methods() {
        let cases = [
            TestClient::get("http://control.test/auth/callback").build(),
            TestClient::get("http://control.test/auth/config").build(),
            TestClient::post("http://control.test/auth/token").build(),
            TestClient::post("http://control.test/auth/refresh").build(),
        ];
        for mut request in cases {
            let root = router();
            let mut path = PathState::from_owned_path(request.uri().path().to_owned());
            assert!(
                root.detect(&mut request, &mut path).await.is_some(),
                "{} {} must resolve to the browser OIDC handler",
                request.method(),
                request.uri().path(),
            );
        }
    }

    #[test]
    fn validates_pkce_verifier_shape() {
        assert!(valid_pkce_verifier(&"a".repeat(43)));
        assert!(valid_pkce_verifier(&format!("{}-._~", "z".repeat(124))));
        assert!(!valid_pkce_verifier(&"a".repeat(42)));
        assert!(!valid_pkce_verifier(&"a".repeat(129)));
        assert!(!valid_pkce_verifier(&format!("{}=", "a".repeat(42))));
    }

    #[test]
    fn accepts_only_https_origins_or_explicit_loopback_http() {
        assert_eq!(
            validate_public_url("https://cloud.example.com/", false).unwrap(),
            "https://cloud.example.com"
        );
        assert!(validate_public_url("http://cloud.example.com", true).is_err());
        assert!(validate_public_url("http://127.0.0.1:5430", false).is_err());
        assert_eq!(
            validate_public_url("http://[::1]:5430", true).unwrap(),
            "http://[::1]:5430"
        );
        assert!(validate_public_url("https://cloud.example.com/path", false).is_err());
        assert!(validate_public_url("https://user@cloud.example.com", false).is_err());
        for address in ["https://0.0.0.0:4321", "https://[::]:4321", "https://0"] {
            assert!(validate_public_url(address, false).is_err());
        }
    }
}
