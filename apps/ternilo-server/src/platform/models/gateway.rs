use std::time::Duration;

use salvo_core::{
    http::{StatusCode, header},
    prelude::{Depot, Json, Request, Response, Router, handler},
};
use serde_json::{Value, json};
use ternilo_control::{
    ModelRequestInput, ModelRequestPermit, ModelRequestSettlement, ModelRequestState,
};
use ternilo_protocol::{HarnessError, ProviderProtocol};
use tokio::sync::mpsc;

use crate::platform::{
    http::now_ms,
    state::{AppState, app_state},
};
use error::GatewayError;
use protocol::PreparedRequest;

mod error;
mod native;
mod protocol;
mod upstream;
use super::usage;

#[cfg(test)]
mod tests;

const MAX_REQUEST_BYTES: usize = 24 * 1024 * 1024;

pub(crate) fn router() -> Router {
    Router::with_path("v1")
        .hoop(crate::platform::identity::no_store)
        .push(Router::with_path("models").get(list_models))
        .push(Router::with_path("chat/completions").post(chat_completion))
        .push(Router::with_path("responses").post(create_response))
        .push(Router::with_path("messages").post(create_message))
        .push(Router::with_path("models/{model_action}").post(generate_content))
        .push(
            Router::with_path("device-account/{device_provider}")
                .push(Router::with_path("models").get(list_models))
                .push(Router::with_path("chat/completions").post(chat_completion))
                .push(Router::with_path("responses").post(create_response))
                .push(Router::with_path("messages").post(create_message))
                .push(Router::with_path("models/{model_action}").post(generate_content)),
        )
        .push(
            Router::with_path("device/{device_grant}")
                .push(Router::with_path("models").get(list_models))
                .push(Router::with_path("chat/completions").post(chat_completion))
                .push(Router::with_path("responses").post(create_response))
                .push(Router::with_path("messages").post(create_message))
                .push(Router::with_path("models/{model_action}").post(generate_content)),
        )
}

#[handler]
async fn list_models(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, GatewayError> {
    let token = model_key(request)?;
    let models = permitted_models(app_state(depot), request, token).await?;
    if request.headers().contains_key("x-goog-api-key") {
        let models = models.into_iter().filter(|model| model.protocol == ProviderProtocol::GoogleGemini).map(|model| json!({
            "name": format!("models/{}", model.model_id), "displayName": model.display_name,
            "inputTokenLimit": model.defaults.context_window, "outputTokenLimit": model.defaults.max_output_tokens,
            "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            "thinking": model.defaults.reasoning.is_some(),
        })).collect::<Vec<_>>();
        return Ok(Json(json!({"models": models})));
    }
    if request.headers().contains_key("x-api-key") {
        let data = models.into_iter().filter(|model| model.protocol == ProviderProtocol::AnthropicMessages).map(|model| json!({
            "id": model.model_id, "type": "model", "display_name": model.display_name,
            "created_at": "1970-01-01T00:00:00Z", "protocol": model.protocol,
            "max_input_tokens": model.defaults.context_window, "max_tokens": model.defaults.max_output_tokens,
            "reasoning": model.defaults.reasoning,
        })).collect::<Vec<_>>();
        return Ok(Json(
            json!({"first_id": data.first().map(|value| &value["id"]), "last_id": data.last().map(|value| &value["id"]), "has_more": false, "data": data}),
        ));
    }
    let data: Vec<_> = models
        .into_iter()
        .map(|model| {
            json!({
                "id": model.model_id,
                "object": "model",
                "created": 0,
                "owned_by": "ternilo",
                "name": model.display_name,
                "protocol": model.protocol,
                "context_window": model.defaults.context_window,
                "max_output_tokens": model.defaults.max_output_tokens,
                "reasoning": model.defaults.reasoning,
                "defaults": model.defaults,
            })
        })
        .collect();
    Ok(Json(json!({"object":"list", "data":data})))
}

#[handler]
async fn chat_completion(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<(), GatewayError> {
    serve(
        request,
        depot,
        response,
        ProviderProtocol::OpenAiChatCompletions,
    )
    .await
}

#[handler]
async fn create_response(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<(), GatewayError> {
    serve(request, depot, response, ProviderProtocol::OpenAiResponses).await
}

#[handler]
async fn create_message(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<(), GatewayError> {
    serve(
        request,
        depot,
        response,
        ProviderProtocol::AnthropicMessages,
    )
    .await
}

#[handler]
async fn generate_content(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<(), GatewayError> {
    serve(request, depot, response, ProviderProtocol::GoogleGemini).await
}

async fn serve(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
    protocol: ProviderProtocol,
) -> Result<(), GatewayError> {
    let state = app_state(depot).clone();
    let (prepared, permit) = accept_request(&state, request, protocol).await?;
    let request_id = permit
        .request
        .request_id
        .parse::<salvo_core::http::HeaderValue>()
        .map_err(|_| HarnessError::execution("invalid request identifier"))?;
    response
        .headers_mut()
        .insert("x-request-id", request_id.clone());
    response
        .headers_mut()
        .insert("x-ternilo-request-id", request_id);
    if !permit.newly_accepted {
        return Err(GatewayError::new(
            StatusCode::CONFLICT,
            "duplicate_request",
            "this Idempotency-Key was already accepted; the upstream request was not repeated",
        ));
    }
    let stream = prepared.stream;
    let (sender, mut receiver) = mpsc::channel(16);
    tokio::spawn(complete(state, permit, prepared, sender));
    match receiver.recv().await {
        Some(Delivery::Start) => {}
        Some(Delivery::Error(error)) => return Err(error),
        _ => {
            return Err(GatewayError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "model_service_error",
                "model request ended without a response",
            ));
        }
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static cache policy"),
    );
    response.headers_mut().insert(
        "x-accel-buffering",
        "no".parse().expect("static proxy policy"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        if stream {
            "text/event-stream; charset=utf-8"
        } else {
            "application/json"
        }
        .parse()
        .expect("static MIME type"),
    );
    response.stream(futures_util::stream::unfold(
        receiver,
        move |mut receiver| async move {
            let bytes = match receiver.recv().await? {
                Delivery::Bytes(bytes) => bytes,
                Delivery::Error(error) if stream => {
                    protocol::sse_frame(&error.stream_value(protocol), protocol)
                }
                Delivery::Error(error) => error.value().to_string().into_bytes(),
                Delivery::Start => return None,
            };
            Some((Ok::<_, std::io::Error>(bytes), receiver))
        },
    ));
    Ok(())
}

async fn accept_request(
    state: &AppState,
    request: &mut Request,
    protocol: ProviderProtocol,
) -> Result<(PreparedRequest, ModelRequestPermit), GatewayError> {
    let token = model_key(request)?.to_owned();
    let request_key = idempotency_key(request)?;
    // Authenticate before reading a potentially large body. Reservation rechecks access atomically.
    let models = permitted_models(state, request, &token).await?;
    let payload = request
        .payload_with_max_size(MAX_REQUEST_BYTES)
        .await
        .map_err(|_| {
            GatewayError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "could not read model request body within the 24 MiB limit",
            )
        })?;
    let mut body: Value = serde_json::from_slice(payload)
        .map_err(|_| HarnessError::invalid("request body must contain valid JSON"))?;
    if protocol == ProviderProtocol::GoogleGemini {
        let action = request
            .param::<String>("model_action")
            .ok_or_else(|| HarnessError::invalid("missing Gemini model action"))?;
        let (model, method) = action.rsplit_once(':').ok_or_else(|| {
            HarnessError::invalid(
                "Gemini path requires model:generateContent or model:streamGenerateContent",
            )
        })?;
        if !matches!(method, "generateContent" | "streamGenerateContent") {
            return Err(HarnessError::invalid("unsupported Gemini method").into());
        }
        let object = body
            .as_object_mut()
            .ok_or_else(|| HarnessError::invalid("request must be a JSON object"))?;
        object.insert("model".to_owned(), json!(model));
        object.insert(
            "stream".to_owned(),
            json!(method == "streamGenerateContent"),
        );
    }
    let mut prepared = PreparedRequest::parse(body, protocol)?;
    let published = models
        .iter()
        .find(|model| model.model_id == prepared.model_id)
        .ok_or_else(|| {
            GatewayError::new(
                StatusCode::FORBIDDEN,
                "model_access_denied",
                "this Key does not have access to the requested model",
            )
        })?;
    if published.protocol.api_protocol() != protocol {
        return Err(GatewayError::new(
            StatusCode::BAD_REQUEST,
            "model_protocol_mismatch",
            "use the protocol published for this model",
        ));
    }
    let reserved_tokens =
        prepared.prepare_upstream(published.protocol, &published.model_id, &published.defaults)?;
    let input = ModelRequestInput {
        request_key,
        payload_hash: prepared.payload_hash.clone(),
        model_id: prepared.model_id.clone(),
        protocol: published.protocol,
        reserved_tokens,
    };
    let permit = if let Some(provider) = request.param::<String>("device_provider") {
        state
            .store
            .reserve_device_account_request(&token, &provider, &input, now_ms()?)
            .await?
    } else if let Some(grant) = request.param::<String>("device_grant") {
        state
            .store
            .reserve_device_model_request(&token, &grant, &input, now_ms()?)
            .await?
    } else {
        state
            .store
            .reserve_model_request(&token, &input, now_ms()?)
            .await?
    };
    Ok((prepared, permit))
}

enum Delivery {
    Start,
    Bytes(Vec<u8>),
    Error(GatewayError),
}

async fn complete(
    state: AppState,
    permit: ModelRequestPermit,
    mut prepared: PreparedRequest,
    output: mpsc::Sender<Delivery>,
) {
    let mut completion = upstream::Completion::default();
    let result = execute_guarded(&state, &permit, &mut prepared, &output, &mut completion).await;
    if result.as_ref().is_err_and(|error| {
        error.code == "request_cancelled"
            || error.status == StatusCode::FORBIDDEN
            || error.status == StatusCode::UNAUTHORIZED
    }) {
        completion.state = ModelRequestState::Cancelled;
    }
    let settlement = ModelRequestSettlement {
        state: completion.state,
        usage: completion.usage,
        upstream_request_id: completion.upstream_request_id,
        error_code: result.as_ref().err().map(|error| error.code.to_owned()),
    };
    let settled = match now_ms() {
        Ok(now) => state
            .store
            .settle_model_request(&permit.request.request_id, &settlement, now)
            .await
            .map(|_| ())
            .map_err(GatewayError::from),
        Err(error) => Err(error.into()),
    };
    // Completion becomes visible only after the accepted request has been settled.
    match settled.and(result) {
        Ok(()) => {
            if !completion.started && output.send(Delivery::Start).await.is_err() {
                return;
            }
            for bytes in completion.tail {
                if output.send(Delivery::Bytes(bytes)).await.is_err() {
                    break;
                }
            }
        }
        Err(error) => {
            let _ = output.send(Delivery::Error(error)).await;
        }
    }
}

async fn execute_guarded(
    state: &AppState,
    permit: &ModelRequestPermit,
    prepared: &mut PreparedRequest,
    output: &mpsc::Sender<Delivery>,
    completion: &mut upstream::Completion,
) -> Result<(), GatewayError> {
    let route = &permit.route;
    if let Some(hosted) = &route.provider.hosted_tools {
        ternilo_builtins::apply_hosted_web_tools(&mut prepared.body, hosted);
    }
    let budget = prepared.prepare_upstream(
        route.provider.protocol,
        &route.upstream_model,
        &route.model.defaults,
    )?;
    if budget > permit.request.reserved_tokens {
        return Err(GatewayError::new(
            StatusCode::CONFLICT,
            "model_configuration_changed",
            "model limits changed while accepting this request; retry with a new Idempotency-Key",
        ));
    }
    let mut client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if route.provider.timeout_ms > 0 {
        client = client.timeout(Duration::from_millis(route.provider.timeout_ms));
    }
    let client = client.build().map_err(|_| {
        GatewayError::upstream(
            "upstream_configuration_error",
            "could not initialize the upstream connection",
        )
    })?;
    let endpoint = ternilo_builtins::provider_model_endpoint(
        &route.provider.base_url,
        route.provider.protocol,
        &route.upstream_model,
        prepared.stream,
    )?;
    let mut shutdown = state.shutdown.clone();
    if output.is_closed() || *shutdown.borrow() {
        return Err(GatewayError::cancelled(
            "client disconnected or Server is stopping",
        ));
    }
    // Finish the acceptance transaction before a periodic authorization query
    // can run. A suspended transaction would deadlock a one-connection pool.
    state
        .store
        .mark_model_request_attempted(&permit.request.request_id, now_ms()?)
        .await?;
    let operation = upstream::execute(
        &client,
        &endpoint,
        route.api_key.as_ref().map(|key| key.as_str()),
        prepared,
        route.provider.protocol,
        output,
        completion,
    );
    tokio::pin!(operation);
    let mut check = tokio::time::interval(Duration::from_secs(2));
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut operation => return result,
            () = output.closed() => return Err(GatewayError::cancelled("client disconnected during the model request")),
            _ = shutdown.changed() => return Err(GatewayError::cancelled("Server is stopping")),
            _ = check.tick() => state.store.check_model_request_authorized(&permit.request.request_id, now_ms()?).await?,
        }
    }
}

fn model_key(request: &Request) -> Result<&str, GatewayError> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            request
                .headers()
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .or_else(|| {
            request
                .headers()
                .get("x-goog-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .filter(|token| !token.is_empty() && !token.chars().any(char::is_whitespace))
        .ok_or_else(|| {
            GatewayError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                "a model API Key is required",
            )
        })
}

fn idempotency_key(request: &Request) -> Result<String, GatewayError> {
    if let Some(value) = request.headers().get("idempotency-key") {
        return value
            .to_str()
            .ok()
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 200
                    && value.bytes().all(|byte| byte.is_ascii_graphic())
            })
            .map(str::to_owned)
            .ok_or_else(|| {
                GatewayError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "Idempotency-Key must contain 1 to 200 visible ASCII characters",
                )
            });
    }
    Ok(protocol::hex(&rand::random::<[u8; 24]>()))
}

async fn permitted_models(
    state: &AppState,
    request: &Request,
    token: &str,
) -> Result<Vec<ternilo_control::PublicModel>, GatewayError> {
    Ok(
        if let Some(provider) = request.param::<String>("device_provider") {
            state
                .store
                .list_device_account_models(token, &provider, now_ms()?)
                .await?
        } else if let Some(grant) = request.param::<String>("device_grant") {
            state
                .store
                .list_device_models(token, &grant, now_ms()?)
                .await?
        } else {
            state.store.list_key_models(token, now_ms()?).await?
        },
    )
}
