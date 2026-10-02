use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use salvo_core::{
    http::header,
    prelude::{Depot, Request, Response, Router, handler},
};
use salvo_extra::size_limiter::max_size;
use ternilo_cloud::{WorkerModelFrame, WorkerModelRequest};
use ternilo_control::ModelRequestState;
use ternilo_kernel::{ModelOutput, RunCancellation};
use ternilo_protocol::{ErrorCode, HarnessError, ModelRetryFailure};
use tokio::sync::mpsc;

use crate::platform::{
    http::{ApiError, bearer_token, invalid_request, now_ms},
    state::app_state,
};
use access::AcceptedModelCall;

mod access;
mod monitor;
mod node_access;
mod observer;
mod route;

#[cfg(test)]
mod tests;

pub(crate) fn router() -> Router {
    Router::with_path("internal/worker/v1/model")
        .hoop(max_size(24 * 1024 * 1024))
        .post(model)
}

#[handler]
async fn model(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<(), ApiError> {
    let state = app_state(depot).clone();
    super::require_managed_execution(&state)?;
    let token = bearer_token(request)?.to_owned();
    let body = request
        .parse_json::<WorkerModelRequest>()
        .await
        .map_err(invalid_request)?;
    let (call, resolved) = AcceptedModelCall::accept(state, token, &body).await?;
    stream_response(response, call, body.request, resolved)
}

pub(crate) fn node_router() -> Router {
    Router::with_path("internal/node/v1/model")
        .hoop(max_size(24 * 1024 * 1024))
        .post(node_model)
}

#[handler]
async fn node_model(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<(), ApiError> {
    let state = app_state(depot).clone();
    let token = bearer_token(request)?.to_owned();
    let body = request
        .parse_json::<ternilo_protocol::NodeModelRequest>()
        .await
        .map_err(invalid_request)?;
    if matches!(
        &body.binding,
        ternilo_protocol::RunModelBinding::ComputerProvider { .. }
    ) {
        return super::computer_models::respond(state, token, body, response).await;
    }
    let (call, resolved) = AcceptedModelCall::accept_node(state, token, &body).await?;
    stream_response(response, call, body.request, resolved)
}

fn stream_response(
    response: &mut Response,
    call: Arc<AcceptedModelCall>,
    request: ternilo_protocol::ModelRequest,
    resolved: route::ResolvedBrokeredRoute,
) -> Result<(), ApiError> {
    let (frames, receiver) = mpsc::channel::<WorkerModelFrame>(64);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-ndjson".parse().expect("static MIME type"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static cache policy"),
    );
    response.headers_mut().insert(
        "x-accel-buffering",
        "no".parse().expect("static proxy policy"),
    );
    response.headers_mut().insert(
        "x-request-id",
        call.request_id.parse().map_err(invalid_request)?,
    );
    response.stream(futures_util::stream::unfold(
        receiver,
        |mut receiver| async move {
            let frame = receiver.recv().await?;
            let data = serde_json::to_vec(&frame).map(|mut bytes| {
                bytes.push(b'\n');
                bytes
            });
            Some((data, receiver))
        },
    ));
    tokio::spawn(complete(call, request, resolved, frames));
    Ok(())
}

async fn complete(
    call: Arc<AcceptedModelCall>,
    request: ternilo_protocol::ModelRequest,
    resolved: route::ResolvedBrokeredRoute,
    frames: mpsc::Sender<WorkerModelFrame>,
) {
    let cancellation = RunCancellation::default();
    let output: Arc<dyn ModelOutput> = Arc::new(StreamOutput {
        frames: frames.clone(),
        cancellation: cancellation.clone(),
    });
    let observer: Arc<dyn ternilo_builtins::ModelAttemptObserver> =
        Arc::new(observer::AttemptObserver {
            call: Arc::clone(&call),
            protocol: resolved.route.protocol,
        });
    let monitor =
        monitor::RequestMonitor::start(Arc::clone(&call), frames.clone(), cancellation.clone());
    // The monitor cancels the token; it never suspends a callback that owns a
    // database transaction. Observed attempt usage is settled even on cancel.
    let mut result = ternilo_builtins::complete_provider_model(
        resolved.route,
        resolved.api_key,
        request,
        output,
        cancellation.clone(),
        Some(observer),
    )
    .await;
    let reason = monitor.finish().await;
    if result.as_ref().is_err_and(HarnessError::is_cancelled)
        && let Some(reason) = reason
    {
        result = Err(reason);
    }
    let state =
        if cancellation.is_cancelled() || result.as_ref().is_err_and(HarnessError::is_cancelled) {
            ModelRequestState::Cancelled
        } else if result.is_ok() {
            ModelRequestState::Completed
        } else {
            ModelRequestState::Failed
        };
    let error_code = result.as_ref().err().map(|error| error.code.to_string());
    let settled = match now_ms() {
        Ok(now) => call.finish(state, error_code.as_deref(), now).await,
        Err(error) => Err(error),
    };
    let frame = match settled.and(result) {
        Ok(mut response) => {
            route::public_provider(&call.binding).clone_into(&mut response.provider);
            call.binding.model_id().clone_into(&mut response.model);
            response.provider_request_id = Some(call.request_id.clone());
            WorkerModelFrame::Complete { response }
        }
        Err(error) => WorkerModelFrame::Error {
            error: visible_error(error),
        },
    };
    if !*call.state.shutdown.borrow() {
        let _ = tokio::time::timeout(Duration::from_secs(2), frames.send(frame)).await;
    }
}

fn visible_error(mut error: HarnessError) -> HarnessError {
    if error.code == ErrorCode::Execution {
        "model request failed; check the Server model request log".clone_into(&mut error.message);
    }
    error
}

struct StreamOutput {
    frames: mpsc::Sender<WorkerModelFrame>,
    cancellation: RunCancellation,
}

impl StreamOutput {
    async fn send(&self, frame: WorkerModelFrame) -> Result<(), HarnessError> {
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => Err(HarnessError::cancelled("model request was cancelled")),
            sent = self.frames.send(frame) => sent.map_err(|_|HarnessError::cancelled("model gateway stream was closed")),
        }
    }
}

impl ModelOutput for StreamOutput {
    fn emit<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.send(WorkerModelFrame::Delta { delta }))
    }
    fn emit_reasoning<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.send(WorkerModelFrame::ReasoningDelta { delta }))
    }
    fn retry_scheduled<'a>(
        &'a self,
        retry: u32,
        max_retries: u32,
        delay_ms: u64,
        failure: ModelRetryFailure,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        let failure = ModelRetryFailure {
            message: "upstream model is temporarily unavailable".to_owned(),
            code: failure.code,
        };
        Box::pin(self.send(WorkerModelFrame::RetryScheduled {
            retry,
            max_retries,
            delay_ms,
            failure,
        }))
    }
    fn retry_started<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.send(WorkerModelFrame::RetryStarted { retry }))
    }
    fn retry_cancelled<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::time::timeout(
                Duration::from_secs(2),
                self.frames.send(WorkerModelFrame::RetryCancelled { retry }),
            )
            .await
            .map_err(|_| {
                HarnessError::cancelled("client stopped reading the cancelled model stream")
            })?
            .map_err(|_| HarnessError::cancelled("model gateway stream was closed"))
        })
    }
}
