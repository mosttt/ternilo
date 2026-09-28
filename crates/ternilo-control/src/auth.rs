use std::time::Duration;

use futures_util::StreamExt;
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header,
    jwk::{Jwk, JwkSet},
};
use serde::Deserialize;
use ternilo_protocol::HarnessError;
use tokio::sync::RwLock;

use crate::OidcPrincipal;

mod identity;
pub use identity::VerifiedOidcIdentity;

const MAX_OIDC_DOCUMENT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_OIDC_DOCUMENT_LENGTH: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OidcConfig {
    pub issuer: String,
    pub audience: String,
    pub allow_insecure_discovery: bool,
    pub jwks_cache_ttl: Duration,
}

impl OidcConfig {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.issuer.trim().is_empty() {
            return Err(HarnessError::invalid("OIDC issuer must not be empty"));
        }
        require_secure_url(&self.issuer, self.allow_insecure_discovery, "OIDC issuer")?;
        if self.jwks_cache_ttl.is_zero() {
            return Err(HarnessError::invalid(
                "OIDC JWKS cache TTL must be positive",
            ));
        }
        Ok(())
    }
}

pub struct OidcAuthenticator {
    config: OidcConfig,
    client: reqwest::Client,
    jwks_uri: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: Option<String>,
    id_token_algorithms: Vec<String>,
    cache: RwLock<JwksCache>,
}

struct JwksCache {
    keys: JwkSet,
    fetched_at: tokio::time::Instant,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: Option<String>,
    id_token_signing_alg_values_supported: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct OidcClaims {
    iss: String,
    sub: String,
    aud: serde_json::Value,
    exp: u64,
    iat: Option<u64>,
    nonce: Option<String>,
    azp: Option<String>,
    at_hash: Option<String>,
    email: Option<String>,
    name: Option<String>,
    preferred_username: Option<String>,
}

impl OidcAuthenticator {
    pub async fn discover(mut config: OidcConfig) -> Result<Self, HarnessError> {
        config.validate()?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| HarnessError::execution(format!("build OIDC HTTP client: {error}")))?;
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            config.issuer.trim_end_matches('/')
        );
        let discovery: DiscoveryDocument =
            fetch_json(&client, &discovery_url, "OIDC discovery document").await?;
        if discovery.issuer.trim_end_matches('/') != config.issuer.trim_end_matches('/') {
            return Err(HarnessError::policy(
                "OIDC discovery issuer does not match configured issuer",
            ));
        }
        require_secure_url(
            &discovery.jwks_uri,
            config.allow_insecure_discovery,
            "OIDC JWKS URI",
        )?;
        require_secure_url(
            &discovery.authorization_endpoint,
            config.allow_insecure_discovery,
            "OIDC authorization endpoint",
        )?;
        require_secure_url(
            &discovery.token_endpoint,
            config.allow_insecure_discovery,
            "OIDC token endpoint",
        )?;
        if let Some(endpoint) = &discovery.userinfo_endpoint {
            require_secure_url(
                endpoint,
                config.allow_insecure_discovery,
                "OIDC UserInfo URI",
            )?;
        }
        config.issuer.clone_from(&discovery.issuer);
        let keys = fetch_json(&client, &discovery.jwks_uri, "OIDC JWKS").await?;
        Ok(Self {
            config,
            client,
            jwks_uri: discovery.jwks_uri,
            authorization_endpoint: discovery.authorization_endpoint,
            token_endpoint: discovery.token_endpoint,
            userinfo_endpoint: discovery.userinfo_endpoint,
            id_token_algorithms: discovery
                .id_token_signing_alg_values_supported
                .unwrap_or_else(|| vec!["RS256".into()]),
            cache: RwLock::new(JwksCache {
                keys,
                fetched_at: tokio::time::Instant::now(),
            }),
        })
    }

    #[must_use]
    pub fn authorization_endpoint(&self) -> &str {
        &self.authorization_endpoint
    }

    #[must_use]
    pub fn token_endpoint(&self) -> &str {
        &self.token_endpoint
    }

    pub async fn authenticate(&self, token: &str) -> Result<OidcPrincipal, HarnessError> {
        if self.config.audience.is_empty() {
            return Err(HarnessError::policy(
                "direct OIDC access tokens are not enabled",
            ));
        }
        let (claims, _) = self
            .verified_claims(token, &self.config.audience, false)
            .await?;
        claims.principal()
    }

    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.config.issuer
    }

    async fn verified_claims(
        &self,
        token: &str,
        audience: &str,
        id_token: bool,
    ) -> Result<(OidcClaims, Algorithm), HarnessError> {
        if token.is_empty() || token.len() > 16 * 1024 {
            return Err(HarnessError::policy("invalid OIDC bearer token length"));
        }
        let header = decode_header(token)
            .map_err(|_| HarnessError::policy("decode OIDC bearer token header"))?;
        if !allowed_algorithm(header.alg) {
            return Err(HarnessError::policy(
                "OIDC bearer token uses a symmetric or unsupported algorithm",
            ));
        }
        if id_token
            && !self
                .id_token_algorithms
                .contains(&format!("{:?}", header.alg))
        {
            return Err(HarnessError::policy(
                "OIDC ID Token signing algorithm is not advertised by the issuer",
            ));
        }
        let kid = header
            .kid
            .as_deref()
            .ok_or_else(|| HarnessError::policy("OIDC bearer token has no key id"))?;
        if self.cache.read().await.fetched_at.elapsed() >= self.config.jwks_cache_ttl {
            self.refresh_keys().await?;
        }
        let mut key = self.find_key(kid).await;
        if key.is_none() {
            self.refresh_keys().await?;
            key = self.find_key(kid).await;
        }
        let key = key.ok_or_else(|| HarnessError::policy("OIDC key id is not trusted"))?;
        let decoding_key = DecodingKey::from_jwk(&key)
            .map_err(|_| HarnessError::policy("OIDC JWK is not a usable signing key"))?;
        let mut validation = Validation::new(header.alg);
        validation.set_audience(&[audience]);
        validation.set_issuer(std::slice::from_ref(&self.config.issuer));
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 30;
        validation.reject_tokens_expiring_in_less_than = 5;
        let token = decode::<OidcClaims>(token, &decoding_key, &validation)
            .map_err(|_| HarnessError::policy("OIDC bearer token validation failed"))?;
        Ok((token.claims, header.alg))
    }

    async fn find_key(&self, kid: &str) -> Option<Jwk> {
        self.cache.read().await.keys.find(kid).cloned()
    }

    async fn refresh_keys(&self) -> Result<(), HarnessError> {
        let keys = fetch_json(&self.client, &self.jwks_uri, "OIDC JWKS").await?;
        *self.cache.write().await = JwksCache {
            keys,
            fetched_at: tokio::time::Instant::now(),
        };
        Ok(())
    }
}

impl OidcClaims {
    fn principal(self) -> Result<OidcPrincipal, HarnessError> {
        let principal = OidcPrincipal {
            issuer: self.iss,
            subject: self.sub,
            email: self.email,
            display_name: self.name.or(self.preferred_username),
        };
        principal.validate()?;
        Ok(principal)
    }
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    label: &str,
) -> Result<T, HarnessError> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| HarnessError::execution(format!("fetch {label}: {error}")))?;
    read_document(response, label).await
}

async fn read_document<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    label: &str,
) -> Result<T, HarnessError> {
    if !response.status().is_success() {
        return Err(HarnessError::execution(format!(
            "fetch {label}: HTTP {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_OIDC_DOCUMENT_BYTES)
    {
        return Err(HarnessError::execution(format!("{label} is too large")));
    }
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk =
            chunk.map_err(|error| HarnessError::execution(format!("read {label}: {error}")))?;
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > MAX_OIDC_DOCUMENT_LENGTH)
        {
            return Err(HarnessError::execution(format!("{label} is too large")));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|error| HarnessError::execution(format!("decode {label}: {error}")))
}

fn require_secure_url(value: &str, allow_insecure: bool, label: &str) -> Result<(), HarnessError> {
    if value.starts_with("https://")
        || (allow_insecure
            && (value.starts_with("http://127.0.0.1:") || value.starts_with("http://localhost:")))
    {
        Ok(())
    } else {
        Err(HarnessError::policy(format!(
            "{label} must use HTTPS; insecure HTTP is restricted to explicit loopback development"
        )))
    }
}

const fn allowed_algorithm(algorithm: Algorithm) -> bool {
    !matches!(
        algorithm,
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde::Serialize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TEST_RSA_MODULUS: &str = "xLcx62j_KkP6oigJs23c0bSt_l0EC6qGzE7Lp6abCDIq5DRCzDVpyXvCccFsOzlbA8j6zrN9dY2u4gOJGE3tzvymY54R9bdNhHyUYh0jc5WCeY2X_VsepfVJblBbcIxHIKPlB3VY8tiE7Djj5xTsSZRSUYMVgSUztLIm3d3mxGublZdCPVhoWIkmx1XGMOfDyH9pkdwyeLYWK7jzj3QJUvzwg7NjBvnDzpHe111c_x7CGTq6Ad4AT4Cc5lhDOKY5cm8WpaNzvGNNAJtr1RDzRXDEWljYDYzpHRonIPTyDwDzy7vASIsKaU4fkPGncJDhs5osJkjyXEIeCgi8aQhy2Q";

    #[derive(Serialize)]
    struct TestClaims<'a> {
        iss: &'a str,
        sub: &'a str,
        aud: &'a str,
        exp: u64,
        nbf: u64,
        email: &'a str,
        name: &'a str,
    }

    fn test_encoding_key() -> EncodingKey {
        let encoded = include_str!("../tests/fixtures/oidc-private.pem")
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect::<String>();
        let der = STANDARD.decode(encoded).unwrap();
        EncodingKey::from_rsa_der(&der)
    }

    #[test]
    fn oidc_never_accepts_shared_secret_algorithms() {
        assert!(!allowed_algorithm(Algorithm::HS256));
        assert!(allowed_algorithm(Algorithm::RS256));
        assert!(allowed_algorithm(Algorithm::ES256));
    }

    #[test]
    fn insecure_discovery_is_loopback_only() {
        assert!(require_secure_url("https://id.example.com", false, "issuer").is_ok());
        assert!(require_secure_url("http://127.0.0.1:5556", true, "issuer").is_ok());
        assert!(require_secure_url("http://id.example.com", true, "issuer").is_err());
    }

    #[tokio::test]
    async fn discovery_and_asymmetric_token_validation_work_end_to_end() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let issuer = format!("http://{address}");
        let discovery = serde_json::json!({
            "issuer": issuer,
            "jwks_uri": format!("http://{address}/jwks"),
            "authorization_endpoint": format!("http://{address}/authorize"),
            "token_endpoint": format!("http://{address}/token")
        })
        .to_string();
        let jwks = serde_json::json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "kid": "test-key",
                "alg": "RS256",
                "n": TEST_RSA_MODULUS,
                "e": "AQAB"
            }]
        })
        .to_string();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 4_096];
                let size = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                let body = if request.starts_with("GET /jwks ") {
                    &jwks
                } else {
                    &discovery
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let authenticator = OidcAuthenticator::discover(OidcConfig {
            issuer: issuer.clone(),
            audience: "ternilo-test".to_owned(),
            allow_insecure_discovery: true,
            jwks_cache_ttl: Duration::from_mins(1),
        })
        .await
        .unwrap();
        assert_eq!(
            authenticator.authorization_endpoint(),
            format!("http://{address}/authorize")
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-key".to_owned());
        let token = encode(
            &header,
            &TestClaims {
                iss: &issuer,
                sub: "oidc-user",
                aud: "ternilo-test",
                exp: now + 3_600,
                nbf: now.saturating_sub(1),
                email: "user@example.com",
                name: "OIDC User",
            },
            &test_encoding_key(),
        )
        .unwrap();
        let principal = authenticator.authenticate(&token).await.unwrap();
        assert_eq!(principal.subject, "oidc-user");
        assert_eq!(principal.email.as_deref(), Some("user@example.com"));

        let wrong_audience = encode(
            &header,
            &TestClaims {
                iss: &issuer,
                sub: "oidc-user",
                aud: "another-service",
                exp: now + 3_600,
                nbf: now.saturating_sub(1),
                email: "user@example.com",
                name: "OIDC User",
            },
            &test_encoding_key(),
        )
        .unwrap();
        assert!(authenticator.authenticate(&wrong_audience).await.is_err());
        server.await.unwrap();
    }
}
