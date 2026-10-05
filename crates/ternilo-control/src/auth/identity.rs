use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha384, Sha512};

use super::{Algorithm, HarnessError, OidcAuthenticator, OidcClaims, OidcPrincipal, read_document};

pub struct VerifiedOidcIdentity {
    pub principal: OidcPrincipal,
    pub expires_at: u64,
    pub username: Option<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    email: Option<String>,
    name: Option<String>,
    preferred_username: Option<String>,
    username: Option<String>,
}

impl OidcAuthenticator {
    pub async fn authenticate_id_token(
        &self,
        token: &str,
        client_id: &str,
        nonce: &str,
        access_token: &str,
        refreshing: bool,
        now_seconds: u64,
    ) -> Result<VerifiedOidcIdentity, HarnessError> {
        let (claims, algorithm) = self.verified_claims(token, client_id, true).await?;
        validate_claims(&claims, client_id, nonce, refreshing, now_seconds)?;
        if let Some(expected) = &claims.at_hash {
            let hash = match algorithm {
                Algorithm::RS256 | Algorithm::PS256 | Algorithm::ES256 => {
                    Sha256::digest(access_token.as_bytes()).to_vec()
                }
                Algorithm::RS384 | Algorithm::PS384 | Algorithm::ES384 => {
                    Sha384::digest(access_token.as_bytes()).to_vec()
                }
                Algorithm::RS512 | Algorithm::PS512 => {
                    Sha512::digest(access_token.as_bytes()).to_vec()
                }
                _ => return Err(HarnessError::policy("unsupported OIDC at_hash algorithm")),
            };
            if URL_SAFE_NO_PAD.encode(&hash[..hash.len() / 2]) != *expected {
                return Err(HarnessError::policy(
                    "OIDC access token does not match ID Token at_hash",
                ));
            }
        }
        Ok(VerifiedOidcIdentity {
            expires_at: claims.exp,
            username: claims
                .preferred_username
                .clone()
                .or(claims.username.clone()),
            principal: claims.principal()?,
        })
    }

    pub async fn userinfo(
        &self,
        access_token: &str,
        mut principal: OidcPrincipal,
        username: Option<String>,
    ) -> Result<(OidcPrincipal, Option<String>), HarnessError> {
        let Some(endpoint) = &self.userinfo_endpoint else {
            return Ok((principal, username));
        };
        let response = self
            .client
            .get(endpoint)
            .bearer_auth(access_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| HarnessError::unavailable("OIDC UserInfo is temporarily unavailable"))?;
        let info: UserInfo = read_document(response, "OIDC UserInfo").await?;
        if info.sub != principal.subject {
            return Err(HarnessError::policy(
                "OIDC UserInfo subject does not match ID Token",
            ));
        }
        if info.email.is_some() {
            principal.email = info.email;
        }
        let username = info
            .preferred_username
            .clone()
            .or(info.username.clone())
            .or(username);
        if let Some(name) = info.name.or(info.preferred_username).or(info.username) {
            principal.display_name = Some(name);
        }
        principal.validate()?;
        Ok((principal, username))
    }
}

fn validate_claims(
    claims: &OidcClaims,
    client_id: &str,
    nonce: &str,
    refreshing: bool,
    now_seconds: u64,
) -> Result<(), HarnessError> {
    if claims
        .iat
        .is_none_or(|issued| issued > now_seconds.saturating_add(30) || issued > claims.exp)
        || claims.exp <= now_seconds
    {
        return Err(HarnessError::policy("OIDC ID Token timestamps are invalid"));
    }
    if claims.nonce.as_deref() != Some(nonce) && !(refreshing && claims.nonce.is_none()) {
        return Err(HarnessError::policy(
            "OIDC ID Token nonce does not match this login",
        ));
    }
    if claims
        .azp
        .as_deref()
        .is_some_and(|party| party != client_id)
        || claims
            .aud
            .as_array()
            .is_some_and(|audiences| audiences.len() > 1)
            && claims.azp.as_deref() != Some(client_id)
    {
        return Err(HarnessError::policy(
            "OIDC ID Token authorized party does not match this client",
        ));
    }
    Ok(())
}
