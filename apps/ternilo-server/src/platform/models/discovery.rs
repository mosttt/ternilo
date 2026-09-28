use salvo_core::prelude::{Depot, Json, Request, handler};
use ternilo_control::PlatformAction;
use ternilo_protocol::{HarnessError, ProviderModel, ProviderModelDiscoveryRequest};

use crate::platform::{
    http::{ApiError, invalid_request},
    state::{actor, app_state},
};

#[handler]
pub(super) async fn discover(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<ProviderModel>>, ApiError> {
    let store = &app_state(depot).store;
    let user = actor(depot);
    store
        .require_platform_action(user, PlatformAction::ModelsManage)
        .await?;
    let discovery = request
        .parse_json::<ProviderModelDiscoveryRequest>()
        .await
        .map_err(invalid_request)?;
    discovery.validate()?;
    let saved = match discovery.provider_id.as_deref() {
        Some(id) => Some(store.get_model_provider(user, id).await?),
        None => None,
    };
    let base_url = discovery
        .base_url
        .as_deref()
        .or_else(|| {
            saved
                .as_ref()
                .map(|provider| provider.profile.base_url.as_str())
        })
        .ok_or_else(|| HarnessError::invalid("provider discovery requires base_url"))?
        .trim()
        .trim_end_matches('/');
    let protocol = discovery
        .protocol
        .or_else(|| saved.as_ref().map(|provider| provider.profile.protocol))
        .unwrap_or_default();
    let timeout_ms = discovery
        .timeout_ms
        .or_else(|| saved.as_ref().map(|provider| provider.profile.timeout_ms))
        .unwrap_or(120_000);
    let api_key = match discovery.api_key.filter(|value| !value.trim().is_empty()) {
        Some(value) => Some(zeroize::Zeroizing::new(value)),
        None => match discovery.provider_id.as_deref() {
            Some(id) => store.resolve_model_provider_secret(user, id).await?,
            None => None,
        },
    };
    let mut client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if timeout_ms > 0 {
        client = client.timeout(std::time::Duration::from_millis(timeout_ms));
    }
    let client = client
        .build()
        .map_err(|_| HarnessError::execution("build model discovery client"))?;
    Ok(Json(
        ternilo_builtins::discover_provider_models(
            &client,
            base_url,
            protocol,
            api_key.as_deref().map(String::as_str),
        )
        .await?,
    ))
}
