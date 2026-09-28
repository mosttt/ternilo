use futures_util::StreamExt;
use ternilo_kernel::ModelOutput;
use ternilo_protocol::{HarnessError, ModelGatewayFrame, ModelResponse};

pub async fn read_model_gateway_response(
    response: reqwest::Response,
    output: &dyn ModelOutput,
) -> Result<ModelResponse, HarnessError> {
    if !response.status().is_success() {
        let status = response.status();
        let error = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|value| value.get("error").cloned())
            .and_then(|error| serde_json::from_value::<HarnessError>(error).ok());
        return Err(error.unwrap_or_else(|| {
            HarnessError::unavailable(format!(
                "Server model gateway rejected the request (HTTP {status})"
            ))
        }));
    }
    let mut chunks = response.bytes_stream();
    let mut line = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| interrupted())?;
        for part in chunk.split_inclusive(|byte| *byte == b'\n') {
            line.extend_from_slice(part);
            if part.ends_with(b"\n") {
                if let Some(response) = emit_model_gateway_line(&line, output).await? {
                    return Ok(response);
                }
                line.clear();
            }
        }
    }
    if !line.is_empty()
        && let Some(response) = emit_model_gateway_line(&line, output).await?
    {
        return Ok(response);
    }
    Err(interrupted())
}

fn interrupted() -> HarnessError {
    HarnessError::execution(
        "Server model connection ended without completion; its result is unknown and was not replayed",
    )
}

async fn emit_model_gateway_line(
    line: &[u8],
    output: &dyn ModelOutput,
) -> Result<Option<ModelResponse>, HarnessError> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let frame: ModelGatewayFrame = serde_json::from_slice(line)
        .map_err(|_| HarnessError::execution("Server model stream contains an invalid frame"))?;
    match frame {
        ModelGatewayFrame::Delta { delta } => output.emit(delta).await?,
        ModelGatewayFrame::ReasoningDelta { delta } => output.emit_reasoning(delta).await?,
        ModelGatewayFrame::RetryScheduled {
            retry,
            max_retries,
            delay_ms,
            failure,
        } => {
            output
                .retry_scheduled(retry, max_retries, delay_ms, failure)
                .await?;
        }
        ModelGatewayFrame::RetryStarted { retry } => output.retry_started(retry).await?,
        ModelGatewayFrame::RetryCancelled { retry } => output.retry_cancelled(retry).await?,
        ModelGatewayFrame::Complete { response } => return Ok(Some(response)),
        ModelGatewayFrame::Error { error } => return Err(error),
    }
    Ok(None)
}
