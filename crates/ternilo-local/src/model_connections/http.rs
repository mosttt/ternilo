use serde::de::DeserializeOwned;
use ternilo_protocol::HarnessError;

pub(super) fn server_url(value: &str) -> Result<String, HarnessError> {
    let mut url = reqwest::Url::parse(value.trim())
        .map_err(|_| HarnessError::invalid("enter the Server URL"))?;
    let loopback = url
        .host_str()
        .is_some_and(|host| host == "localhost" || host == "127.0.0.1" || host == "[::1]");
    if (url.scheme() != "https" && !(loopback && url.scheme() == "http"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(HarnessError::invalid(
            "Server URL must use HTTPS (HTTP is allowed for loopback), without credentials, a path, query or fragment",
        ));
    }
    url.set_path("");
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

async fn response(request: reqwest::RequestBuilder) -> Result<reqwest::Response, HarnessError> {
    let response = request.send().await.map_err(|error| {
        HarnessError::execution(format!("connect to model Server: {}", error.without_url()))
    })?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(HarnessError::policy(
            "model Server authorization is invalid, expired or revoked; reconnect or check account access",
        ));
    }
    if !response.status().is_success() {
        return Err(HarnessError::execution(format!(
            "model Server returned HTTP {}; check the connection, account and model allowance",
            response.status().as_u16()
        )));
    }
    Ok(response)
}

pub(super) async fn json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, HarnessError> {
    let response = response(request).await?;
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        HarnessError::execution(format!(
            "read model Server response: {}",
            error.without_url()
        ))
    })? {
        if bytes.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
            return Err(HarnessError::invalid("model Server response exceeds 2 MiB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| HarnessError::execution(format!("invalid model Server response: {error}")))
}
pub(super) async fn empty(request: reqwest::RequestBuilder) -> Result<(), HarnessError> {
    response(request).await.map(|_| ())
}
