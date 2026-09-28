use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use ternilo_protocol::HarnessError;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinHandle,
};

use super::{LocalApplication, ModelSelection};

/// An HTTP model fixture that returns the text it actually receives, including
/// resolved Skill instructions and attachments. It exercises the real adapter.
pub(super) struct TestModel {
    base_url: String,
    task: JoinHandle<()>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl TestModel {
    pub(super) async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (body_start, content_length) = loop {
                    let mut chunk = [0; 4096];
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0, "model request contains complete headers");
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        break (end + 4, length);
                    }
                };
                while bytes.len() < body_start + content_length {
                    let mut chunk = [0; 4096];
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0, "model request contains its complete body");
                    bytes.extend_from_slice(&chunk[..read]);
                }
                let request: Value =
                    serde_json::from_slice(&bytes[body_start..body_start + content_length])
                        .unwrap();
                captured_requests.lock().unwrap().push(request.clone());
                let content = &request["messages"].as_array().unwrap().last().unwrap()["content"];
                let text = content.as_str().map_or_else(
                    || {
                        content
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    },
                    str::to_owned,
                );
                let text = if request["messages"][0]["content"]
                    .as_str()
                    .is_some_and(|prompt| {
                        prompt.starts_with("You name software-agent conversations.")
                    }) {
                    let conversation: Value = serde_json::from_str(&text).unwrap();
                    conversation["request"].as_str().unwrap().to_owned()
                } else {
                    text
                };
                let body = format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({"choices": [{"delta": {"content": text}}]})
                );
                stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        Self {
            base_url,
            task,
            requests,
        }
    }

    pub(super) fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    pub(super) async fn install(
        &self,
        application: &LocalApplication,
        session_id: &str,
    ) -> Result<(), HarnessError> {
        application
            .update_model(
                session_id,
                ModelSelection::OpenAiCompatible {
                    base_url: self.base_url.clone(),
                    model: "text-fixture".to_owned(),
                    api_key_env: None,
                    timeout_ms: 5000,
                    max_attempts: 1,
                    retry_base_delay_ms: 10,
                },
            )
            .await?;
        Ok(())
    }
}

impl Drop for TestModel {
    fn drop(&mut self) {
        self.task.abort();
    }
}
