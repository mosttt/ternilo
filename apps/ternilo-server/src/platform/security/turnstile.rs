use std::time::Duration;

use serde::{Deserialize, Serialize};
use ternilo_protocol::HarnessError;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::platform) struct TurnstileSettings {
    pub site_key: String,
    #[serde(default)]
    pub secret_key: Option<String>,
}

#[derive(Deserialize)]
struct Verification {
    success: bool,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    action: String,
}

impl TurnstileSettings {
    pub(super) fn validate(&self) -> Result<(), HarnessError> {
        let secret = self.secret_key.as_deref().unwrap_or_default();
        if self.site_key.is_empty()
            || self.site_key.len() > 256
            || !self
                .site_key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || secret.trim().is_empty()
            || secret.len() > 4096
            || secret.chars().any(char::is_control)
        {
            return Err(HarnessError::invalid(
                "Turnstile site key and secret key are required and must be valid",
            ));
        }
        Ok(())
    }

    pub(super) async fn verify(
        &self,
        token: Option<&str>,
        action: &str,
        public_url: &str,
    ) -> Result<(), HarnessError> {
        self.verify_at(
            token,
            action,
            public_url,
            "https://challenges.cloudflare.com/turnstile/v0/siteverify",
        )
        .await
    }

    async fn verify_at(
        &self,
        token: Option<&str>,
        action: &str,
        public_url: &str,
        endpoint: &str,
    ) -> Result<(), HarnessError> {
        let token = token
            .filter(|token| !token.is_empty() && token.len() <= 2048)
            .ok_or_else(|| HarnessError::policy("complete the Turnstile verification"))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                HarnessError::unavailable("Turnstile verification is temporarily unavailable")
            })?;
        let mut response = client
            .post(endpoint)
            .form(&[
                ("secret", self.secret_key.as_deref().unwrap_or_default()),
                ("response", token),
            ])
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| {
                HarnessError::unavailable("Turnstile verification is temporarily unavailable")
            })?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            HarnessError::unavailable("Turnstile verification is temporarily unavailable")
        })? {
            if bytes.len() + chunk.len() > 16 * 1024 {
                return Err(HarnessError::unavailable(
                    "Turnstile verification response is too large",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let result: Verification = serde_json::from_slice(&bytes).map_err(|_| {
            HarnessError::unavailable("Turnstile verification is temporarily unavailable")
        })?;
        let origin = reqwest::Url::parse(public_url)
            .map_err(|_| HarnessError::execution("Turnstile public URL is invalid"))?;
        validate_result(&result, action, origin.host_str().unwrap_or_default())
    }
}

fn validate_result(
    result: &Verification,
    action: &str,
    hostname: &str,
) -> Result<(), HarnessError> {
    if !result.success || result.action != action || !result.hostname.eq_ignore_ascii_case(hostname)
    {
        return Err(HarnessError::policy(
            "Turnstile verification failed; please try again",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn siteverify_is_called_and_rejection_or_invalid_responses_fail_closed() {
        let settings = TurnstileSettings {
            site_key: "site-key".into(),
            secret_key: Some("private-key".into()),
        };
        for (body, accepted) in [
            (
                r#"{"success":true,"hostname":"login.example.test","action":"login"}"#,
                true,
            ),
            (
                r#"{"success":false,"error-codes":["timeout-or-duplicate"]}"#,
                false,
            ),
            (
                r#"{"success":true,"hostname":"another.example.test","action":"login"}"#,
                false,
            ),
            (
                r#"{"success":true,"hostname":"login.example.test","action":"register"}"#,
                false,
            ),
            ("not-json", false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/siteverify", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0_u8; 4096];
                    let length = stream.read(&mut buffer).await.unwrap();
                    assert!(length > 0);
                    request.extend_from_slice(&buffer[..length]);
                    let text = String::from_utf8_lossy(&request);
                    if text.contains("secret=private-key&response=valid-token") {
                        break;
                    }
                    assert!(request.len() < 8192);
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
            let result = settings
                .verify_at(
                    Some("valid-token"),
                    "login",
                    "https://login.example.test",
                    &endpoint,
                )
                .await;
            assert_eq!(result.is_ok(), accepted);
            server.await.unwrap();
        }
        assert!(
            settings
                .verify_at(
                    None,
                    "login",
                    "https://login.example.test",
                    "http://127.0.0.1:1"
                )
                .await
                .is_err()
        );
        assert!(
            settings
                .verify_at(
                    Some("token"),
                    "login",
                    "https://login.example.test",
                    "http://127.0.0.1:1"
                )
                .await
                .is_err()
        );
    }

    #[test]
    fn verification_requires_success_and_matching_origin_and_action() {
        let mut result = Verification {
            success: true,
            hostname: "login.example.test".into(),
            action: "login".into(),
        };
        assert!(validate_result(&result, "login", "login.example.test").is_ok());
        assert!(validate_result(&result, "register", "login.example.test").is_err());
        assert!(validate_result(&result, "login", "other.example.test").is_err());
        result.success = false;
        assert!(validate_result(&result, "login", "login.example.test").is_err());
    }
}
