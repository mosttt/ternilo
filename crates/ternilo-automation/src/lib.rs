#![forbid(unsafe_code)]

use std::{collections::BTreeMap, sync::Arc, time::UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_local::LocalApplication;
use ternilo_protocol::{Attachment, HarnessError, SessionId, WorkspaceId};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    sync::{Mutex, broadcast, mpsc},
};

pub const AUTOMATION_PROTOCOL_VERSION: u32 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeParams {
    #[serde(default = "protocol_version")]
    protocol_version: u32,
    #[serde(default)]
    client: Option<ClientInfo>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientInfo {
    name: String,
    #[serde(rename = "version", default)]
    _version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewSessionParams {
    workspace_id: Option<WorkspaceId>,
    workspace_path: Option<String>,
    session_id: Option<String>,
    agent_id: Option<String>,
    parent_session_id: Option<SessionId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionParams {
    session_id: SessionId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptParams {
    session_id: SessionId,
    prompt: String,
    run_id: Option<String>,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelParams {
    session_id: SessionId,
    run_id: String,
}

#[derive(Clone)]
struct Output {
    sender: mpsc::Sender<Value>,
}

impl Output {
    async fn response(&self, id: Value, result: Value) {
        let _ = self
            .sender
            .send(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .await;
    }

    async fn error(&self, id: Value, code: i32, message: impl Into<String>, data: Option<Value>) {
        let mut error = json!({ "code": code, "message": message.into() });
        if let Some(data) = data {
            error["data"] = data;
        }
        let _ = self
            .sender
            .send(json!({ "jsonrpc": "2.0", "id": id, "error": error }))
            .await;
    }

    async fn notification(&self, method: &str, params: Value) {
        let _ = self
            .sender
            .send(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await;
    }
}

#[derive(Default)]
struct ServerState {
    initialized: bool,
    next_run: u64,
    active: Arc<Mutex<BTreeMap<String, String>>>,
}

enum Control {
    Continue,
    Shutdown,
}

pub async fn serve_stdio(application: Arc<LocalApplication>) -> Result<(), HarnessError> {
    serve(application, tokio::io::stdin(), tokio::io::stdout()).await
}

pub async fn serve<R, W>(
    application: Arc<LocalApplication>,
    reader: R,
    writer: W,
) -> Result<(), HarnessError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (sender, mut receiver) = mpsc::channel::<Value>(256);
    let output = Output { sender };
    let writer_task = tokio::spawn(async move {
        let mut writer = BufWriter::new(writer);
        while let Some(message) = receiver.recv().await {
            let mut bytes = serde_json::to_vec(&message).map_err(|error| {
                HarnessError::execution(format!("serialize JSON-RPC output: {error}"))
            })?;
            bytes.push(b'\n');
            writer.write_all(&bytes).await.map_err(|error| {
                HarnessError::execution(format!("write JSON-RPC output: {error}"))
            })?;
            writer.flush().await.map_err(|error| {
                HarnessError::execution(format!("flush JSON-RPC output: {error}"))
            })?;
        }
        Ok::<(), HarnessError>(())
    });

    let mut lines = BufReader::new(reader).lines();
    let mut state = ServerState::default();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| HarnessError::execution(format!("read JSON-RPC input: {error}")))?
    {
        if line.trim().is_empty() {
            continue;
        }
        let request = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) if request.jsonrpc == "2.0" => request,
            Ok(_) => {
                output
                    .error(Value::Null, -32600, "jsonrpc must equal 2.0", None)
                    .await;
                continue;
            }
            Err(error) => {
                output
                    .error(
                        Value::Null,
                        -32700,
                        "invalid JSON-RPC request",
                        Some(json!({ "detail": error.to_string() })),
                    )
                    .await;
                continue;
            }
        };
        if matches!(
            handle_request(&application, &output, &mut state, request).await,
            Control::Shutdown
        ) {
            break;
        }
    }

    let active = state.active.lock().await.clone();
    for (run_id, session_id) in active {
        let _ = application.cancel_turn(&session_id, &run_id).await;
    }
    application.shutdown().await?;
    drop(output);
    writer_task
        .await
        .map_err(|error| HarnessError::execution(format!("join JSON-RPC writer: {error}")))??;
    Ok(())
}

async fn handle_request(
    application: &Arc<LocalApplication>,
    output: &Output,
    state: &mut ServerState,
    request: JsonRpcRequest,
) -> Control {
    let Some(id) = request.id else {
        return Control::Continue;
    };
    if request.method != "initialize" && !state.initialized {
        output
            .error(id, -32002, "initialize must be called first", None)
            .await;
        return Control::Continue;
    }
    let result = match request.method.as_str() {
        "initialize" => initialize(state, request.params),
        "ping" => Ok(json!({ "ok": true })),
        "session/list" => serde_json::to_value(application.snapshot().await)
            .map_err(|error| HarnessError::execution(format!("serialize state: {error}"))),
        "session/new" => new_session(application, request.params).await,
        "session/prompt" => {
            prompt(application, output, state, request.params, id.clone()).await;
            return Control::Continue;
        }
        "session/cancel" => cancel(application, request.params).await,
        "session/events" => session_events(application, request.params).await,
        "session/status" => session_status(state, request.params).await,
        "session/close" => close_session(application, request.params).await,
        "shutdown" => {
            output.response(id, json!({ "accepted": true })).await;
            return Control::Shutdown;
        }
        _ => {
            output
                .error(
                    id,
                    -32601,
                    format!("unknown method {}", request.method),
                    None,
                )
                .await;
            return Control::Continue;
        }
    };
    match result {
        Ok(value) => output.response(id, value).await,
        Err(error) => harness_error(output, id, error).await,
    }
    Control::Continue
}

fn initialize(state: &mut ServerState, params: Value) -> Result<Value, HarnessError> {
    let params: InitializeParams = parse_params(params)?;
    if params.protocol_version != AUTOMATION_PROTOCOL_VERSION {
        return Err(HarnessError::invalid(format!(
            "unsupported automation protocol {}; expected {AUTOMATION_PROTOCOL_VERSION}",
            params.protocol_version
        )));
    }
    if let Some(client) = params.client
        && client.name.trim().is_empty()
    {
        return Err(HarnessError::invalid("client name must not be empty"));
    }
    state.initialized = true;
    Ok(json!({
        "protocol_version": AUTOMATION_PROTOCOL_VERSION,
        "server": { "name": "ternilo", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": {
            "session_list": true,
            "session_fork": true,
            "prompt_receipts": true,
            "event_notifications": true,
            "cancellation": true,
            "attachments": true
        }
    }))
}

async fn new_session(application: &LocalApplication, params: Value) -> Result<Value, HarnessError> {
    let params: NewSessionParams = parse_params(params)?;
    if params.workspace_id.is_some() == params.workspace_path.is_some() {
        return Err(HarnessError::invalid(
            "session/new requires exactly one of workspace_id or workspace_path",
        ));
    }
    let workspace_id = if let Some(workspace_id) = params.workspace_id {
        workspace_id
    } else {
        application
            .add_workspace(params.workspace_path.as_deref().expect("checked above"))
            .await?
            .workspace_id
    };
    let session = if let Some(parent) = params.parent_session_id {
        let parent_session = application
            .snapshot()
            .await
            .sessions
            .into_iter()
            .find(|session| session.identity.session_id == parent)
            .ok_or_else(|| HarnessError::invalid(format!("unknown parent session {parent}")))?;
        if parent_session.workspace_id != workspace_id {
            return Err(HarnessError::invalid(
                "forked session must use its parent workspace",
            ));
        }
        application
            .fork_session(parent.as_str(), params.session_id, params.agent_id)
            .await?
    } else {
        application
            .create_session(workspace_id, params.session_id, params.agent_id)
            .await?
    };
    serde_json::to_value(session)
        .map_err(|error| HarnessError::execution(format!("serialize new session: {error}")))
}

async fn prompt(
    application: &Arc<LocalApplication>,
    output: &Output,
    state: &mut ServerState,
    params: Value,
    id: Value,
) {
    let params = match parse_params::<PromptParams>(params) {
        Ok(params) => params,
        Err(error) => {
            harness_error(output, id, error).await;
            return;
        }
    };
    if params.prompt.trim().is_empty() {
        harness_error(
            output,
            id,
            HarnessError::invalid("prompt must not be empty"),
        )
        .await;
        return;
    }
    let run_id = params.run_id.unwrap_or_else(|| {
        state.next_run = state.next_run.saturating_add(1);
        format!("rpc-run-{}-{}", now_ms(), state.next_run)
    });
    if state.active.lock().await.contains_key(&run_id) {
        harness_error(
            output,
            id,
            HarnessError::invalid(format!("run id {run_id:?} is already active")),
        )
        .await;
        return;
    }
    let session_id = params.session_id.as_str().to_owned();
    let event_receiver = application.subscribe_events();
    let cursor = match application.events(&session_id).await {
        Ok(events) => events.last().map(|event| event.seq),
        Err(error) => {
            harness_error(output, id, error).await;
            return;
        }
    };
    state
        .active
        .lock()
        .await
        .insert(run_id.clone(), session_id.clone());
    output
        .response(
            id,
            json!({ "accepted": true, "session_id": session_id, "run_id": run_id }),
        )
        .await;
    output
        .notification(
            "session.status",
            json!({ "session_id": session_id, "run_id": run_id, "status": "running" }),
        )
        .await;
    let application = Arc::clone(application);
    let output = output.clone();
    let active = Arc::clone(&state.active);
    tokio::spawn(async move {
        stream_run(
            application,
            output,
            active,
            session_id,
            run_id,
            params.prompt,
            params.attachments,
            event_receiver,
            cursor,
        )
        .await;
    });
}

#[allow(clippy::too_many_arguments)]
async fn stream_run(
    application: Arc<LocalApplication>,
    output: Output,
    active: Arc<Mutex<BTreeMap<String, String>>>,
    session_id: String,
    run_id: String,
    prompt: String,
    attachments: Vec<Attachment>,
    mut event_receiver: broadcast::Receiver<ternilo_local::LocalEventNotification>,
    mut cursor: Option<u64>,
) {
    let turn = application.run_turn_with_attachments(
        &session_id,
        Some(run_id.clone()),
        prompt,
        attachments,
    );
    tokio::pin!(turn);
    let outcome = loop {
        tokio::select! {
            biased;
            result = &mut turn => break result,
            notification = event_receiver.recv() => {
                match notification {
                    Ok(notification) if notification.session_id == session_id
                        && cursor.is_none_or(|seq| notification.event.seq > seq) => {
                        cursor = Some(notification.event.seq);
                        emit_event(&output, &session_id, &run_id, notification.event).await;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break Err(
                        HarnessError::execution("local event notification bus closed during run"),
                    ),
                }
            }
        }
    };
    let _ = emit_events(&application, &output, &session_id, &run_id, cursor).await;
    active.lock().await.remove(&run_id);
    match outcome {
        Ok(outcome) => {
            output
                .notification(
                    "session.status",
                    json!({
                        "session_id": session_id,
                        "run_id": run_id,
                        "status": "idle",
                        "result": {
                            "answer": outcome.answer,
                            "steps": outcome.steps,
                            "tool_calls": outcome.tool_calls,
                        },
                    }),
                )
                .await;
        }
        Err(error) => {
            let status = if error.is_cancelled() {
                "cancelled"
            } else {
                "failed"
            };
            output
                .notification(
                    "session.status",
                    json!({
                        "session_id": session_id,
                        "run_id": run_id,
                        "status": status,
                        "error": { "code": error.code.to_string(), "message": error.message },
                    }),
                )
                .await;
        }
    }
}

async fn emit_event(
    output: &Output,
    session_id: &str,
    run_id: &str,
    event: ternilo_protocol::SessionEvent,
) {
    output
        .notification(
            "session.event",
            json!({ "session_id": session_id, "run_id": run_id, "event": event }),
        )
        .await;
}

async fn emit_events(
    application: &LocalApplication,
    output: &Output,
    session_id: &str,
    run_id: &str,
    cursor: Option<u64>,
) -> Option<u64> {
    let Ok(events) = application.events_after(session_id, cursor).await else {
        return cursor;
    };
    let mut next = cursor;
    for event in events {
        next = Some(event.seq);
        output
            .notification(
                "session.event",
                json!({ "session_id": session_id, "run_id": run_id, "event": event }),
            )
            .await;
    }
    next
}

async fn cancel(application: &LocalApplication, params: Value) -> Result<Value, HarnessError> {
    let params: CancelParams = parse_params(params)?;
    application
        .cancel_turn(params.session_id.as_str(), &params.run_id)
        .await?;
    Ok(json!({ "accepted": true }))
}

async fn session_events(
    application: &LocalApplication,
    params: Value,
) -> Result<Value, HarnessError> {
    let params: SessionParams = parse_params(params)?;
    serde_json::to_value(application.events(params.session_id.as_str()).await?)
        .map_err(|error| HarnessError::execution(format!("serialize session events: {error}")))
}

async fn session_status(state: &ServerState, params: Value) -> Result<Value, HarnessError> {
    let params: SessionParams = parse_params(params)?;
    let active = state.active.lock().await;
    let runs = active
        .iter()
        .filter(|(_, session)| session.as_str() == params.session_id.as_str())
        .map(|(run, _)| run.clone())
        .collect::<Vec<_>>();
    Ok(json!({
        "session_id": params.session_id,
        "status": if runs.is_empty() { "idle" } else { "running" },
        "active_runs": runs,
    }))
}

async fn close_session(
    application: &LocalApplication,
    params: Value,
) -> Result<Value, HarnessError> {
    let params: SessionParams = parse_params(params)?;
    application
        .delete_session(params.session_id.as_str())
        .await?;
    Ok(json!({ "closed": true }))
}

fn parse_params<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, HarnessError> {
    serde_json::from_value(params)
        .map_err(|error| HarnessError::invalid(format!("invalid method params: {error}")))
}

async fn harness_error(output: &Output, id: Value, error: HarnessError) {
    output
        .error(
            id,
            -32000,
            error.message.clone(),
            Some(json!({ "code": error.code.to_string() })),
        )
        .await;
}

const fn protocol_version() -> u32 {
    AUTOMATION_PROTOCOL_VERSION
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};
    use tempfile::TempDir;
    use ternilo_kernel::HostPolicy;
    use ternilo_protocol::RunLimits;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::serve;

    async fn application(
        root: &TempDir,
    ) -> Result<Arc<ternilo_local::LocalApplication>, ternilo_protocol::HarnessError> {
        Ok(Arc::new(
            ternilo_local::LocalApplication::open(
                ternilo_local::catalog()?,
                ternilo_local::local_profile(),
                HostPolicy::local(RunLimits::default()),
                root.path().join("data"),
            )
            .await?,
        ))
    }

    async fn write_request(
        writer: &mut tokio::io::DuplexStream,
        request: Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        writer
            .write_all(serde_json::to_string(&request)?.as_bytes())
            .await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }

    async fn read_frame(
        reader: &mut tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), reader.next_line())
            .await??
            .ok_or("automation server closed before the expected frame")?;
        Ok(serde_json::from_str(&line)?)
    }

    #[tokio::test]
    async fn ndjson_protocol_creates_and_runs_a_session_with_pure_frames()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = TempDir::new()?;
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace)?;
        let app = application(&root).await?;
        let (mut request_writer, request_reader) = tokio::io::duplex(64 * 1024);
        let (response_writer, response_reader) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(serve(app, request_reader, response_writer));
        let mut frames = BufReader::new(response_reader).lines();

        write_request(
            &mut request_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocol_version": 1, "client": { "name": "test" } }
            }),
        )
        .await?;
        let initialized = read_frame(&mut frames).await?;
        assert_eq!(initialized["id"], 1);
        assert_eq!(initialized["result"]["protocol_version"], 1);

        write_request(
            &mut request_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/new",
                "params": { "workspace_path": workspace }
            }),
        )
        .await?;
        let created = read_frame(&mut frames).await?;
        let session_id = created["result"]["identity"]["session_id"]
            .as_str()
            .ok_or("missing session id")?
            .to_owned();

        write_request(
            &mut request_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "session/prompt",
                "params": { "session_id": session_id, "prompt": "/write proof.txt automation proof" }
            }),
        )
        .await?;
        let receipt = read_frame(&mut frames).await?;
        assert_eq!(receipt["id"], 3);
        assert_eq!(receipt["result"]["accepted"], true);

        let mut saw_tool_result = false;
        loop {
            let frame = read_frame(&mut frames).await?;
            if frame["method"] == "session.event"
                && frame["params"]["event"]["type"] == "tool_call_finished"
            {
                saw_tool_result = true;
            }
            if frame["method"] == "session.status" {
                assert_ne!(frame["params"]["status"], "failed", "{frame}");
                if frame["params"]["status"] == "idle" {
                    break;
                }
            }
        }
        assert!(saw_tool_result);
        assert_eq!(
            std::fs::read_to_string(workspace.join("proof.txt"))?,
            "automation proof"
        );

        write_request(
            &mut request_writer,
            json!({ "jsonrpc": "2.0", "id": 4, "method": "shutdown" }),
        )
        .await?;
        let shutdown = read_frame(&mut frames).await?;
        assert_eq!(shutdown["id"], 4);
        drop(request_writer);
        server.await??;
        Ok(())
    }
}
