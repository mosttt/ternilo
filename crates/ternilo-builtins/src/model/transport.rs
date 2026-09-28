//! HTTP attempts, cancellation, retries and SSE framing shared by model protocols.

use super::{
    ModelAttemptObserver, ModelAttemptReport, ProviderModel, StreamCompletion, StreamEvent,
    protocols,
};
use crate::{model_request_digest, read_provider_response};
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use ternilo_kernel::{ModelOutput, RunCancellation};
use ternilo_protocol::{
    HarnessError, ModelRequest, ModelResponse, ModelRetryFailure, ProviderProtocol,
};

impl ProviderModel {
    pub(super) async fn complete_request(
        &self,
        mut request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
        observer: Option<Arc<dyn ModelAttemptObserver>>,
    ) -> Result<ModelResponse, HarnessError> {
        let request_digest = model_request_digest(&request)?;
        if let Some(attachments) = &self.attachments {
            for message in &mut request.messages {
                for attachment in &mut message.attachments {
                    *attachment = attachments.resolve(attachment.clone()).await?;
                }
            }
        }
        let body = protocols::request_body(self, &request)?;
        let api_key = self.resolve_api_key().await?;
        for attempt in 1..=self.max_attempts {
            cancellation.check()?;
            if let Some(observer) = &observer {
                observer.before_attempt(attempt).await?;
            }
            let mut report = ModelAttemptReport::new(attempt);
            let result = self
                .complete_attempt(
                    &body,
                    api_key.as_deref(),
                    output.as_ref(),
                    &cancellation,
                    &mut report,
                )
                .await;
            report.error = result.as_ref().err().cloned();
            let retry = report.retryable.then(|| ModelRetryFailure {
                message: report.error.as_ref().map_or_else(
                    || "model attempt failed".to_owned(),
                    |error| error.message.clone(),
                ),
                code: Some(
                    report
                        .http_status
                        .map_or_else(|| "transport".to_owned(), |status| format!("http_{status}")),
                ),
            });
            if let Some(observer) = &observer {
                observer.after_attempt(report).await?;
            }
            match result {
                Ok(mut response) => {
                    if self.protocol == ProviderProtocol::GoogleGemini {
                        for call in &mut response.tool_calls {
                            if let Some(index) = call.id.strip_prefix("gemini-call-") {
                                call.id = format!("gemini-call-{request_digest}-{index}");
                            }
                        }
                    }
                    response.attempts = attempt;
                    response.request_digest = Some(request_digest);
                    return Ok(response);
                }
                Err(error) => {
                    if let Some(failure) = retry.filter(|_| attempt < self.max_attempts) {
                        self.wait_for_retry(output.as_ref(), attempt, failure, &cancellation)
                            .await?;
                    } else {
                        return Err(error);
                    }
                }
            }
        }
        Err(HarnessError::execution(
            "model retry loop ended without a response",
        ))
    }

    async fn complete_attempt(
        &self,
        body: &Value,
        api_key: Option<&str>,
        output: &dyn ModelOutput,
        cancellation: &RunCancellation,
        report: &mut ModelAttemptReport,
    ) -> Result<ModelResponse, HarnessError> {
        let response = self
            .send_request(body, api_key, cancellation, report)
            .await?;
        let is_event_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let mut completion = if is_event_stream {
            decode_stream(
                response,
                self.protocol,
                &self.provider,
                &self.model,
                output,
                cancellation,
                report,
            )
            .await?
        } else {
            let bytes = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(cancelled_model_error()),
                result = read_provider_response(response, "model") => result?,
            };
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                report.observe(&value);
            }
            let completion =
                protocols::decode_completion(&bytes, self.protocol, &self.provider, &self.model)?;
            if let Some(reasoning) = completion
                .reasoning_content
                .as_ref()
                .filter(|reasoning| !reasoning.is_empty())
            {
                output.emit_reasoning(reasoning.clone()).await?;
            }
            if !completion.content.is_empty() {
                output.emit(completion.content.clone()).await?;
            }
            cancellation.check()?;
            completion
        };
        completion
            .provider_request_id
            .clone_from(&report.upstream_request_id);
        Ok(completion)
    }

    async fn resolve_api_key(&self) -> Result<Option<String>, HarnessError> {
        if let Some(value) = &self.api_key_override {
            return Ok(Some(value.clone()));
        }
        let Some(variable) = self.api_key_env.as_deref() else {
            return Ok(None);
        };
        let environment = self
            .environment
            .as_ref()
            .ok_or_else(|| HarnessError::execution("model credential resolver is unavailable"))?;
        let value = environment
            .resolve_secret(variable.to_owned())
            .await?
            .ok_or_else(|| {
                HarnessError::execution(format!("model credential {variable:?} is not configured"))
            })?;
        Ok(Some(value))
    }

    async fn send_request(
        &self,
        body: &Value,
        api_key: Option<&str>,
        cancellation: &RunCancellation,
        report: &mut ModelAttemptReport,
    ) -> Result<reqwest::Response, HarnessError> {
        cancellation.check()?;
        let outgoing = crate::provider_request(
            self.client.post(&self.endpoint).json(body),
            self.protocol,
            api_key,
        );
        let sent = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled_model_error()),
            result = outgoing.send() => result,
        };
        let response = match sent {
            Ok(response) => response,
            Err(error) => {
                report.retryable = true;
                return Err(HarnessError::execution(format!(
                    "model request failed on attempt {}: {error}",
                    report.attempt
                )));
            }
        };
        report.upstream_request_id = response
            .headers()
            .get("x-request-id")
            .or_else(|| response.headers().get("request-id"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let status = response.status();
        report.http_status = Some(status.as_u16());
        if status.is_success() {
            return Ok(response);
        }
        let bytes = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled_model_error()),
            result = read_provider_response(response, "model") => result?,
        };
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            report.observe(&value);
        }
        report.retryable =
            status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
        Err(HarnessError::execution(format!(
            "model endpoint returned {status}: {}",
            retained_text(&bytes)
        )))
    }

    pub(super) async fn wait_for_retry(
        &self,
        output: &dyn ModelOutput,
        retry: u32,
        failure: ModelRetryFailure,
        cancellation: &RunCancellation,
    ) -> Result<(), HarnessError> {
        let delay = retry_delay(self.retry_base_delay_ms, retry);
        let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        output
            .retry_scheduled(
                retry,
                self.max_attempts.saturating_sub(1),
                delay_ms,
                failure,
            )
            .await?;
        match wait_retry(delay, cancellation).await {
            Ok(()) => output.retry_started(retry).await,
            Err(error) => {
                output.retry_cancelled(retry).await?;
                Err(error)
            }
        }
    }
}

async fn decode_stream(
    response: reqwest::Response,
    protocol: ProviderProtocol,
    provider: &str,
    model: &str,
    output: &dyn ModelOutput,
    cancellation: &RunCancellation,
    report: &mut ModelAttemptReport,
) -> Result<ModelResponse, HarnessError> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut completion = StreamCompletion::default();
    let mut done = false;
    while !done {
        let next = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled_model_error()),
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|error| {
            if error.is_timeout() {
                HarnessError::execution("model request timed out while reading the response; increase the Provider total request timeout; received content was retained")
            } else {
                HarnessError::execution(format!("read streaming model response: {}", error.without_url()))
            }
        })?;
        buffer.extend_from_slice(&chunk);
        while let Some(event) = take_sse_event(&mut buffer) {
            match decode_stream_event_observed(&event, protocol, &mut completion, Some(report))? {
                StreamEvent::Deltas { reasoning, text } => {
                    if !reasoning.is_empty() {
                        output.emit_reasoning(reasoning).await?;
                    }
                    if !text.is_empty() {
                        output.emit(text).await?;
                    }
                }
                StreamEvent::Done => {
                    done = true;
                    break;
                }
                StreamEvent::Metadata => {}
            }
        }
    }
    if !buffer.iter().all(u8::is_ascii_whitespace) {
        match decode_stream_event_observed(&buffer, protocol, &mut completion, Some(report))? {
            StreamEvent::Deltas { reasoning, text } => {
                if !reasoning.is_empty() {
                    output.emit_reasoning(reasoning).await?;
                }
                if !text.is_empty() {
                    output.emit(text).await?;
                }
            }
            StreamEvent::Done => done = true,
            StreamEvent::Metadata => {}
        }
    }
    cancellation.check()?;
    if protocol == ProviderProtocol::GoogleGemini && completion.finish_reason.is_some() {
        done = true;
    }
    if !done {
        return Err(HarnessError::execution(
            "model stream ended before its completion event",
        ));
    }
    completion.finish(provider, model)
}

pub(super) fn take_sse_event(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (start, delimiter) = buffer.windows(2).enumerate().find_map(|(index, pair)| {
        if pair == b"\n\n" {
            Some((index, 2))
        } else if index + 3 < buffer.len() && &buffer[index..index + 4] == b"\r\n\r\n" {
            Some((index, 4))
        } else {
            None
        }
    })?;
    let event = buffer[..start].to_vec();
    buffer.drain(..start + delimiter);
    Some(event)
}

#[cfg(test)]
pub(super) fn decode_stream_event(
    event: &[u8],
    protocol: ProviderProtocol,
    completion: &mut StreamCompletion,
) -> Result<StreamEvent, HarnessError> {
    decode_stream_event_observed(event, protocol, completion, None)
}

fn decode_stream_event_observed(
    event: &[u8],
    protocol: ProviderProtocol,
    completion: &mut StreamCompletion,
    report: Option<&mut ModelAttemptReport>,
) -> Result<StreamEvent, HarnessError> {
    let event = std::str::from_utf8(event)
        .map_err(|error| HarnessError::execution(format!("model stream is not UTF-8: {error}")))?;
    let data = event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return Ok(StreamEvent::Metadata);
    }
    if data.trim() == "[DONE]" {
        if matches!(
            protocol,
            ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages
        ) {
            return Err(HarnessError::execution(
                "native model stream used an OpenAI terminator",
            ));
        }
        return Ok(StreamEvent::Done);
    }
    let value: Value = serde_json::from_str(&data)
        .map_err(|error| HarnessError::execution(format!("parse model stream event: {error}")))?;
    if let Some(report) = report {
        report.observe(&value);
    }
    protocols::decode_stream_data(value, protocol, completion)
}

fn retry_delay(base_ms: u64, attempt: u32) -> std::time::Duration {
    let exponent = attempt.saturating_sub(1).min(6);
    std::time::Duration::from_millis(base_ms.saturating_mul(1_u64 << exponent))
}

async fn wait_retry(
    delay: std::time::Duration,
    cancellation: &RunCancellation,
) -> Result<(), HarnessError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(cancelled_model_error()),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

fn cancelled_model_error() -> HarnessError {
    HarnessError::cancelled("model request was cancelled")
}

fn retained_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().take(4_000).collect()
}
