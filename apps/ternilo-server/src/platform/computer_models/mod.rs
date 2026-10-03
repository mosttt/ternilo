use super::{
    edge::{ComputerModelEvent, ComputerModelStream},
    http::{ApiError, now_ms},
    state::AppState,
};
use salvo_core::{http::header, prelude::Response};
use sha2::{Digest as _, Sha256};
use std::{fmt::Write as _, time::Duration};
use ternilo_control::{ControlUser, NodeModelPrincipal, NodePrincipal};
use ternilo_protocol::{
    ComputerModelAttempt, ComputerModelRequest, DefaultModelSelection, HarnessError,
    ModelGatewayFrame, NodeModelRequest, ProviderModelCatalog as _, ProviderModelDefaults,
    ProviderProfile, RunModelBinding, RunModelSnapshot, SessionId, TenantId,
};
use ternilo_storage::database_error;
use ternilo_transport::{ApplicationOperation, ExecutorId};
use tokio::sync::mpsc;

pub(crate) async fn profile(
    state: &AppState,
    binding: &RunModelBinding,
) -> Result<ProviderProfile, HarnessError> {
    let RunModelBinding::ComputerProvider {
        tenant_id,
        executor_id,
        provider_id,
        ..
    } = binding
    else {
        return Err(HarnessError::policy("expected a source computer"));
    };
    let mut tx = state.store.database().begin().await?;
    state
        .store
        .require_computer_model_source_in(&mut tx, binding)
        .await?;
    tx.commit().await.map_err(database_error)?;
    let value = state
        .edge
        .call_with_timeout(
            tenant_id,
            &ExecutorId::new(executor_id),
            ApplicationOperation::ProviderList,
            Duration::from_secs(15),
        )
        .await?;
    let profiles: Vec<ProviderProfile> = serde_json::from_value(value).map_err(|_| {
        HarnessError::execution("source computer returned an invalid model catalog")
    })?;
    let profile = profiles
        .into_iter()
        .find(|profile| &profile.id == provider_id)
        .ok_or_else(|| {
            HarnessError::unavailable("selected Provider is unavailable on the source computer")
        })?;
    // Connected Server catalogs must never become recursively forwarded sources.
    if is_connected_server_provider(provider_id) {
        return Err(HarnessError::policy(
            "connected Server models cannot be forwarded through a computer",
        ));
    }
    profile.validate()?;
    if let Some(reference) = &profile.api_key_ref {
        let value = state
            .edge
            .call_with_timeout(
                tenant_id,
                &ExecutorId::new(executor_id),
                ApplicationOperation::CredentialList,
                Duration::from_secs(15),
            )
            .await?;
        let inventory: ternilo_protocol::CredentialInventory = serde_json::from_value(value)
            .map_err(|_| {
                HarnessError::execution("source computer returned invalid credential metadata")
            })?;
        if !inventory
            .references
            .iter()
            .any(|entry| &entry.reference == reference && entry.configured)
        {
            return Err(HarnessError::unavailable(
                "the model credential is unavailable on the source computer",
            ));
        }
    }
    Ok(profile)
}

fn is_connected_server_provider(id: &str) -> bool {
    id.strip_prefix("server_")
        .is_some_and(|id| id.len() == 56 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        || id
            .strip_prefix("server_a_")
            .is_some_and(|id| id.len() == 54 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub(crate) async fn resolve_selection(
    state: &AppState,
    user: &ControlUser,
    tenant: &TenantId,
    session: &SessionId,
    previous: Option<&RunModelSnapshot>,
    selection: &DefaultModelSelection,
) -> Result<RunModelSnapshot, HarnessError> {
    selection.validate()?;
    let DefaultModelSelection::ComputerProvider {
        executor_id,
        provider_id,
        model,
        reasoning_effort,
    } = selection
    else {
        return Err(HarnessError::invalid("expected a computer model selection"));
    };
    let existing = previous.filter(|snapshot| matches!(&snapshot.binding, RunModelBinding::ComputerProvider {
        tenant_id: selected_tenant, executor_id: selected_computer, provider_id: selected_provider, model: selected_model, ..
    } if selected_tenant == tenant && selected_computer == executor_id && selected_provider == provider_id && selected_model == model));
    let owner = if let Some(snapshot) = existing {
        snapshot.binding.beneficiary_user_id().clone()
    } else {
        let computer = state
            .store
            .owned_executor(user, tenant, &ExecutorId::new(executor_id))
            .await?;
        let _ = computer;
        user.user_id.clone()
    };
    state
        .store
        .require_edge_model_owner_access(&owner, tenant, session)
        .await?;
    let binding = RunModelBinding::ComputerProvider {
        tenant_id: tenant.clone(),
        owner_user_id: owner,
        executor_id: executor_id.clone(),
        provider_id: provider_id.clone(),
        model: model.clone(),
    };
    let computer_name = state.store.computer_model_source_name(&binding).await?;
    let provider = profile(state, &binding).await?;
    let model = provider.resolved_model(model)?;
    model.reasoning_value(*reasoning_effort)?;
    Ok(RunModelSnapshot {
        binding,
        protocol: provider.protocol,
        defaults: ProviderModelDefaults {
            context_window: model.context_window,
            max_output_tokens: model.max_output_tokens,
            reasoning: model.reasoning,
        },
        reasoning_effort: *reasoning_effort,
        display_name: model.display_name.unwrap_or(model.id),
        source_name: computer_name,
    })
}

struct AcceptedCall {
    state: AppState,
    node: NodePrincipal,
    identity: NodeModelRequest,
    principal: NodeModelPrincipal,
    id: String,
}

impl AcceptedCall {
    async fn accept(
        state: AppState,
        token: &str,
        mut body: NodeModelRequest,
    ) -> Result<(Self, ComputerModelRequest), ternilo_control::ModelAccessError> {
        let node = state.store.authenticate_node(token, now_ms()?).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !state
            .store
            .node_model_request_registered(&node, &body)
            .await?
        {
            if tokio::time::Instant::now() >= deadline {
                return Err(HarnessError::unavailable(
                    "the remote session has not synchronized with Server",
                )
                .into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut tx = state.store.database().begin().await?;
        let initial = state
            .store
            .authorize_node_model_in(&mut tx, &node, &body)
            .await?;
        tx.commit().await.map_err(database_error)?;
        let provider = profile(&state, &initial.snapshot.binding).await?;
        if provider.protocol != initial.snapshot.protocol {
            return Err(HarnessError::policy("the selected model protocol changed").into());
        }
        provider.resolved_model(initial.snapshot.binding.model_id())?;
        let mut canonical = serde_json::to_value(&body.request)
            .map_err(|_| HarnessError::invalid("invalid model request"))?;
        canonical.sort_all_objects();
        let digest = Sha256::digest(canonical.to_string().as_bytes())
            .iter()
            .fold(String::with_capacity(64), |mut value, byte| {
                write!(value, "{byte:02x}").expect("write digest");
                value
            });
        let mut tx = state.store.database().begin().await?;
        let principal = state
            .store
            .authorize_node_model_in(&mut tx, &node, &body)
            .await?;
        if principal != initial {
            return Err(HarnessError::policy(
                "model authorization changed during source discovery",
            )
            .into());
        }
        let id = state
            .store
            .accept_computer_model_request_in(
                &mut tx,
                &principal,
                &body.request_id,
                &digest,
                provider.max_attempts,
                now_ms()?,
            )
            .await?;
        state
            .store
            .begin_computer_model_attempt_in(&mut tx, &principal, &id, 1, now_ms()?)
            .await?;
        tx.commit().await.map_err(database_error)?;
        let request = ComputerModelRequest {
            provider_id: provider.id,
            model: principal.snapshot.binding.model_id().to_owned(),
            protocol: principal.snapshot.protocol,
            defaults: principal.snapshot.defaults.clone(),
            reasoning_effort: principal.snapshot.reasoning_effort,
            max_attempts: provider.max_attempts,
            request: body.request.clone(),
        };
        body.request.messages.clear();
        body.request.tools.clear();
        body.request.system_prompt.clear();
        Ok((
            Self {
                state,
                node,
                identity: body,
                principal,
                id,
            },
            request,
        ))
    }

    async fn check(&self, retry: Option<u32>) -> Result<(), HarnessError> {
        let mut tx = self.state.store.database().begin().await?;
        let current = self
            .state
            .store
            .authorize_node_model_in(&mut tx, &self.node, &self.identity)
            .await?;
        if current != self.principal {
            return Err(HarnessError::policy("the computer model authority changed"));
        }
        self.state
            .store
            .require_computer_model_source_in(&mut tx, &current.snapshot.binding)
            .await?;
        self.state
            .store
            .renew_computer_model_request_in(&mut tx, &current.tenant_id, &self.id, now_ms()?)
            .await?;
        if let Some(attempt) = retry {
            self.state
                .store
                .begin_computer_model_attempt_in(&mut tx, &current, &self.id, attempt, now_ms()?)
                .await?;
        }
        tx.commit().await.map_err(database_error)
    }

    async fn complete(
        &self,
        request: ComputerModelRequest,
        output: &mpsc::Sender<ModelGatewayFrame>,
    ) -> Result<ternilo_protocol::ModelResponse, HarnessError> {
        let RunModelBinding::ComputerProvider {
            tenant_id,
            executor_id,
            owner_user_id,
            ..
        } = &self.principal.snapshot.binding
        else {
            unreachable!()
        };
        let mut stream = self
            .state
            .edge
            .start_computer_model(
                tenant_id,
                &ExecutorId::new(executor_id),
                owner_user_id,
                request,
            )
            .await?;
        let result = self.receive(&mut stream, output).await;
        if result.is_err() {
            stream.cancel().await;
            // Cancellation can still carry the source's final observed usage.
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                while let Some(event) = stream.events.recv().await {
                    match event {
                        ComputerModelEvent::Attempt(
                            report @ ComputerModelAttempt::Finished { .. },
                        ) => {
                            let _ = self
                                .state
                                .store
                                .finish_computer_model_attempt(
                                    tenant_id,
                                    &self.id,
                                    &report,
                                    now_ms()?,
                                )
                                .await;
                        }
                        ComputerModelEvent::Output(frame) if frame.is_terminal() => break,
                        _ => {}
                    }
                }
                Ok::<_, HarnessError>(())
            })
            .await;
        }
        result
    }

    async fn receive(
        &self,
        stream: &mut ComputerModelStream,
        output: &mpsc::Sender<ModelGatewayFrame>,
    ) -> Result<ternilo_protocol::ModelResponse, HarnessError> {
        let mut check = tokio::time::interval(Duration::from_secs(2));
        check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut shutdown = self.state.shutdown.clone();
        loop {
            let event = tokio::select! {
                () = output.closed() => return Err(HarnessError::cancelled("execution computer stopped reading model output")),
                _ = shutdown.changed() => return Err(HarnessError::cancelled("Server is stopping")),
                _ = check.tick() => { self.check(None).await?; continue; },
                event = stream.events.recv() => event.ok_or_else(|| HarnessError::unavailable("source computer model stream ended; this request will not be replayed"))?,
            };
            match event {
                ComputerModelEvent::Attempt(ComputerModelAttempt::Started { attempt }) => {
                    if attempt > 1 {
                        let allowed = self.check(Some(attempt)).await;
                        stream
                            .permit_retry(attempt, allowed.as_ref().err().cloned())
                            .await?;
                        allowed?;
                    }
                }
                ComputerModelEvent::Attempt(report @ ComputerModelAttempt::Finished { .. }) => {
                    self.state
                        .store
                        .finish_computer_model_attempt(
                            &self.principal.tenant_id,
                            &self.id,
                            &report,
                            now_ms()?,
                        )
                        .await?;
                }
                ComputerModelEvent::Output(frame) => match *frame {
                    ModelGatewayFrame::Complete { mut response } => {
                        response.provider_request_id = Some(self.id.clone());
                        return Ok(response);
                    }
                    ModelGatewayFrame::Error { error } => return Err(error),
                    frame => {
                        let send = output.send(frame);
                        tokio::pin!(send);
                        loop {
                            tokio::select! {
                                result = &mut send => { result.map_err(|_| HarnessError::cancelled("execution computer disconnected"))?; break; },
                                _ = check.tick() => self.check(None).await?,
                                _ = shutdown.changed() => return Err(HarnessError::cancelled("Server is stopping")),
                            }
                        }
                    }
                },
            }
        }
    }
}

pub(crate) async fn respond(
    state: AppState,
    token: String,
    body: NodeModelRequest,
    response: &mut Response,
) -> Result<(), ApiError> {
    let (call, request) = AcceptedCall::accept(state, &token, body).await?;
    let (output, events) = mpsc::channel(64);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-ndjson".parse().expect("MIME"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("cache policy"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().expect("proxy policy"));
    response.stream(futures_util::stream::unfold(
        events,
        |mut events| async move {
            let frame: ModelGatewayFrame = events.recv().await?;
            let bytes = serde_json::to_vec(&frame).map(|mut bytes| {
                bytes.push(b'\n');
                bytes
            });
            Some((bytes, events))
        },
    ));
    tokio::spawn(async move {
        let result = call.complete(request, &output).await;
        let state = if result.is_ok() {
            "completed"
        } else if result.as_ref().is_err_and(HarnessError::is_cancelled) {
            "cancelled"
        } else {
            "failed"
        };
        let code = result.as_ref().err().map(|error| error.code.to_string());
        let settled = match now_ms() {
            Ok(now) => {
                call.state
                    .store
                    .finish_computer_model_request(
                        &call.principal.tenant_id,
                        &call.id,
                        state,
                        code.as_deref(),
                        now,
                    )
                    .await
            }
            Err(error) => Err(error),
        };
        let frame = match settled.and(result) {
            Ok(response) => ModelGatewayFrame::Complete { response },
            Err(error) => ModelGatewayFrame::Error { error },
        };
        let _ = output.send(frame).await;
    });
    Ok(())
}
