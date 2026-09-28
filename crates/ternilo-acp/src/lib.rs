#![forbid(unsafe_code)]

use std::{collections::BTreeMap, fmt::Write as _, sync::Arc};

use agent_client_protocol::schema::{ProtocolVersion, v1 as acp};
use agent_client_protocol::{Agent, ConnectionTo, Responder, Stdio};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ternilo_local::{LocalApplication, LocalEventNotification};
use ternilo_protocol::{Attachment, HarnessError, SessionEventKind};
use tokio::sync::{Mutex, broadcast};

#[derive(Clone)]
struct ActivePrompt {
    run_id: String,
    cancel_requested: bool,
}

struct AcpState {
    application: Arc<LocalApplication>,
    owned_sessions: Mutex<BTreeMap<String, String>>,
    active: Mutex<BTreeMap<String, ActivePrompt>>,
}

impl AcpState {
    fn new(application: Arc<LocalApplication>) -> Arc<Self> {
        Arc::new(Self {
            application,
            owned_sessions: Mutex::new(BTreeMap::new()),
            active: Mutex::new(BTreeMap::new()),
        })
    }
}

pub async fn serve_stdio(application: Arc<LocalApplication>) -> Result<(), HarnessError> {
    let state = AcpState::new(Arc::clone(&application));
    let initialize_state = Arc::clone(&state);
    let authenticate_state = Arc::clone(&state);
    let new_session_state = Arc::clone(&state);
    let prompt_state = Arc::clone(&state);
    let cancel_state = Arc::clone(&state);

    let result = Agent
        .builder()
        .name("ternilo-acp")
        .on_receive_request(
            async move |request: acp::InitializeRequest,
                        responder: Responder<acp::InitializeResponse>,
                        _connection: ConnectionTo<agent_client_protocol::Client>| {
                let _keep_state_alive = &initialize_state;
                let negotiated = if request.protocol_version == ProtocolVersion::V1 {
                    ProtocolVersion::V1
                } else {
                    ProtocolVersion::LATEST
                };
                responder.respond(
                    acp::InitializeResponse::new(negotiated)
                        .agent_info(acp::Implementation::new(
                            "ternilo-acp",
                            env!("CARGO_PKG_VERSION"),
                        ))
                        .agent_capabilities(
                            acp::AgentCapabilities::new().prompt_capabilities(
                                acp::PromptCapabilities::new()
                                    .image(true)
                                    .audio(false)
                                    .embedded_context(false),
                            ),
                        ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_request: acp::AuthenticateRequest,
                        responder: Responder<acp::AuthenticateResponse>,
                        _connection: ConnectionTo<agent_client_protocol::Client>| {
                let _keep_state_alive = &authenticate_state;
                responder.respond(acp::AuthenticateResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::NewSessionRequest,
                        responder: Responder<acp::NewSessionResponse>,
                        _connection: ConnectionTo<agent_client_protocol::Client>| {
                match create_session(&new_session_state, request).await {
                    Ok(response) => responder.respond(response),
                    Err(error) => responder.respond_with_error(error),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::PromptRequest,
                        responder: Responder<acp::PromptResponse>,
                        connection: ConnectionTo<agent_client_protocol::Client>| {
                match run_prompt(&prompt_state, request, &connection).await {
                    Ok(response) => responder.respond(response),
                    Err(error) => responder.respond_with_error(error),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: acp::CancelNotification,
                        _connection: ConnectionTo<agent_client_protocol::Client>| {
                cancel_prompt(&cancel_state, notification).await;
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(Stdio::new())
        .await;

    let shutdown = application.shutdown().await;
    result.map_err(|error| HarnessError::execution(format!("ACP transport failed: {error}")))?;
    shutdown
}

async fn create_session(
    state: &AcpState,
    request: acp::NewSessionRequest,
) -> Result<acp::NewSessionResponse, acp::Error> {
    if !request.cwd.is_absolute() {
        return Err(invalid("session/new cwd must be absolute"));
    }
    if !request.additional_directories.is_empty() {
        return Err(invalid("additionalDirectories are not supported"));
    }
    if !request.mcp_servers.is_empty() {
        return Err(invalid("per-session MCP servers are not supported"));
    }
    let cwd = request
        .cwd
        .to_str()
        .ok_or_else(|| invalid("session/new cwd must be valid UTF-8"))?;
    let workspace = state
        .application
        .add_workspace(cwd)
        .await
        .map_err(|error| internal(&error))?;
    let session = state
        .application
        .create_session(workspace.workspace_id, None, None)
        .await
        .map_err(|error| internal(&error))?;
    let session_id = session.identity.session_id.as_str().to_owned();
    state
        .owned_sessions
        .lock()
        .await
        .insert(session_id.clone(), workspace.path);
    Ok(acp::NewSessionResponse::new(acp::SessionId::new(
        session_id,
    )))
}

async fn run_prompt(
    state: &AcpState,
    request: acp::PromptRequest,
    connection: &ConnectionTo<agent_client_protocol::Client>,
) -> Result<acp::PromptResponse, acp::Error> {
    let session_id = request.session_id.0.as_ref().to_owned();
    if !state.owned_sessions.lock().await.contains_key(&session_id) {
        return Err(invalid("unknown or unowned ACP session"));
    }
    let (prompt, attachments) = admit_prompt(request.prompt)?;
    let run_id = format!("acp-{}", now_ms());
    {
        let mut active = state.active.lock().await;
        if active.contains_key(&session_id) {
            return Err(invalid("a prompt is already in flight for this session"));
        }
        active.insert(
            session_id.clone(),
            ActivePrompt {
                run_id: run_id.clone(),
                cancel_requested: false,
            },
        );
    }

    let mut events = state.application.subscribe_events();
    let run = state.application.run_turn_with_attachments(
        &session_id,
        Some(run_id.clone()),
        prompt,
        attachments,
    );
    tokio::pin!(run);
    let outcome = loop {
        tokio::select! {
            biased;
            result = &mut run => break result,
            event = events.recv() => {
                match event {
                    Ok(event) if event.session_id == session_id && event.event.run_id.as_str() == run_id => {
                        emit_committed_message(connection, &session_id, event)?;
                        if state.active.lock().await.get(&session_id).is_some_and(|active| active.cancel_requested) {
                            let _ = state.application.cancel_turn(&session_id, &run_id).await;
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        break Err(HarnessError::execution("local event notification bus closed"));
                    }
                }
            }
        }
    };
    let cancelled = state
        .active
        .lock()
        .await
        .remove(&session_id)
        .is_some_and(|active| active.cancel_requested);
    match outcome {
        Ok(_) if cancelled => Ok(acp::PromptResponse::new(acp::StopReason::Cancelled)),
        Ok(_) => Ok(acp::PromptResponse::new(acp::StopReason::EndTurn)),
        Err(error) if error.is_cancelled() || cancelled => {
            Ok(acp::PromptResponse::new(acp::StopReason::Cancelled))
        }
        Err(error) => Err(internal(&error)),
    }
}

fn admit_prompt(blocks: Vec<acp::ContentBlock>) -> Result<(String, Vec<Attachment>), acp::Error> {
    let mut text = String::new();
    let mut attachments = Vec::new();
    for (index, block) in blocks.into_iter().enumerate() {
        match block {
            acp::ContentBlock::Text(block) => text.push_str(&block.text),
            acp::ContentBlock::Image(block) => {
                if !matches!(
                    block.mime_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                ) {
                    return Err(invalid("ACP images must be PNG, JPEG, WebP, or GIF"));
                }
                let name = block
                    .uri
                    .unwrap_or_else(|| format!("acp-image-{}", index + 1));
                write!(text, "[image name={name}]").expect("writing to a String cannot fail");
                attachments.push(Attachment {
                    name,
                    media_type: block.mime_type.clone(),
                    content: format!("data:{};base64,{}", block.mime_type, block.data),
                });
            }
            acp::ContentBlock::ResourceLink(block) => {
                write!(
                    text,
                    "[resource_link name={} uri={}]",
                    block.name, block.uri
                )
                .expect("writing to a String cannot fail");
            }
            acp::ContentBlock::Audio(_) | acp::ContentBlock::Resource(_) => {
                return Err(invalid(
                    "ACP audio and embedded resources are not supported",
                ));
            }
            _ => return Err(invalid("unsupported ACP prompt content")),
        }
    }
    if text.trim().is_empty() && attachments.is_empty() {
        return Err(invalid("ACP prompt must not be empty"));
    }
    for attachment in &attachments {
        attachment
            .validate()
            .map_err(|error| invalid_harness(&error))?;
        let encoded = attachment
            .content
            .split_once(',')
            .map(|(_, value)| value)
            .ok_or_else(|| invalid("ACP image payload is malformed"))?;
        STANDARD
            .decode(encoded)
            .map_err(|_| invalid("ACP image payload is not valid base64"))?;
    }
    Ok((text, attachments))
}

fn emit_committed_message(
    connection: &ConnectionTo<agent_client_protocol::Client>,
    session_id: &str,
    notification: LocalEventNotification,
) -> Result<(), acp::Error> {
    let SessionEventKind::AssistantMessage { response, .. } = notification.event.kind else {
        return Ok(());
    };
    if response.content.is_empty() {
        return Ok(());
    }
    connection.send_notification(acp::SessionNotification::new(
        acp::SessionId::new(session_id.to_owned()),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(response.content),
        ))),
    ))
}

async fn cancel_prompt(state: &AcpState, notification: acp::CancelNotification) {
    let session_id = notification.session_id.0.as_ref().to_owned();
    let run_id = {
        let mut active = state.active.lock().await;
        let Some(active) = active.get_mut(&session_id) else {
            return;
        };
        active.cancel_requested = true;
        active.run_id.clone()
    };
    let _ = state.application.cancel_turn(&session_id, &run_id).await;
}

fn invalid(message: impl Into<String>) -> acp::Error {
    acp::Error::invalid_params().data(serde_json::json!(message.into()))
}

fn invalid_harness(error: &HarnessError) -> acp::Error {
    invalid(error.to_string())
}

fn internal(error: &HarnessError) -> acp::Error {
    acp::Error::internal_error().data(serde_json::json!(error.to_string()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}
