use futures_util::StreamExt;
use ternilo_kernel::ModelOutput;
use ternilo_protocol::{HarnessError, ModelGatewayFrame, ModelResponse};

pub async fn read_model_gateway_response(
    response: reqwest::Response,
    output: &dyn ModelOutput,
) -> Result<ModelResponse, HarnessError> {
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value.starts_with("mrq_")
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .map(str::to_owned);
    read_response(response, output).await.map_err(|mut error| {
        if let Some(id) = request_id {
            error.message.push_str("; Server request ID: ");
            error.message.push_str(&id);
        }
        error
    })
}

async fn read_response(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, pin::Pin, sync::Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Default)]
    struct Output(Mutex<Vec<String>>);
    impl ModelOutput for Output {
        fn emit<'a>(
            &'a self,
            delta: String,
        ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(delta);
                Ok(())
            })
        }
    }

    async fn response(body: &'static str, id: &'static str) -> reqwest::Response {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            assert!(socket.read(&mut buffer).await.unwrap() > 0);
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nX-Request-Id: {id}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        reqwest::get(format!("http://{address}")).await.unwrap()
    }

    #[tokio::test]
    async fn heartbeat_is_not_output_and_incomplete_stream_retains_correlation() {
        let output = Output::default();
        let body = "\n \n{\"type\":\"delta\",\"delta\":\"partial\"}\n\n";
        let error = read_model_gateway_response(response(body, "mrq_test-1").await, &output)
            .await
            .unwrap_err();
        assert!(
            error
                .message
                .contains("result is unknown and was not replayed")
        );
        assert!(error.message.ends_with("; Server request ID: mrq_test-1"));
        assert_eq!(*output.0.lock().unwrap(), ["partial"]);
    }

    #[tokio::test]
    async fn terminal_error_keeps_its_category_and_rejects_noncanonical_correlation_headers() {
        let body = "\n{\"type\":\"error\",\"error\":{\"code\":\"policy_denied\",\"message\":\"access changed\"}}\n";
        for id in ["mrq_safe", "private-url/secret", "mrq_invalid: value"] {
            let error = read_model_gateway_response(response(body, id).await, &Output::default())
                .await
                .unwrap_err();
            assert_eq!(error.code, ternilo_protocol::ErrorCode::PolicyDenied);
            assert_eq!(
                error.message,
                if id == "mrq_safe" {
                    "access changed; Server request ID: mrq_safe"
                } else {
                    "access changed"
                }
            );
        }
    }
}
