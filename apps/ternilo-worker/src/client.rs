//! Worker transport carries canonical leases without database or model-provider credentials.

use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, de::DeserializeOwned};
use ternilo_cloud::{
    ClaimedCloudSessionCommand, ClaimedCloudTelemetry, CloudRunClaim, CloudRunState,
    CloudSessionRecord, CloudWorkerIdentity, RunAdmission, StartedRun, TelemetryLease,
    TerminalState, WorkerConfiguration, WorkerModelRequest, WorkerRegisterRequest,
    WorkerRegistration, WorkerReply, WorkerRequest, WorkerRpcRequest, WorkerSubagentRun,
    WorkerTeamRequest,
};
use ternilo_kernel::{ModelOutput, RunCancellation};
use ternilo_protocol::{
    AcceptedSubagentRun, Attachment, HarnessError, ModelRequest, ModelResponse, Profile,
    ReferenceContext, RunId, RunOutcome, SessionEvent, SessionId, SessionSubmission,
    SubagentSessionMetadata, UserAnswer, UserQuestion,
};
use ternilo_transport::CommandReply;
use zeroize::Zeroizing;

#[derive(Clone)]
pub(crate) struct WorkerClient {
    http: reqwest::Client,
    origin: reqwest::Url,
    token: Arc<Zeroizing<String>>,
    identity: Arc<RwLock<Option<CloudWorkerIdentity>>>,
    lease_seconds: Arc<std::sync::atomic::AtomicU64>,
    lease_expires_at_ms: Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: HarnessError,
}

pub(crate) struct WorkerInspection {
    pub session: CloudSessionRecord,
    pub profile: Option<Profile>,
    pub extensions: Vec<ternilo_extension::ExtensionDistribution>,
}

impl WorkerClient {
    pub fn new(server_url: &str, token: String) -> Result<Self, HarnessError> {
        let origin = reqwest::Url::parse(server_url)
            .map_err(|_| HarnessError::invalid("Worker Server URL is invalid"))?;
        if !matches!(origin.scheme(), "http" | "https")
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || !matches!(origin.path(), "" | "/")
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(HarnessError::invalid(
                "Worker Server URL must be an HTTP or HTTPS origin",
            ));
        }
        if token.is_empty()
            || token
                .chars()
                .any(|value| value.is_whitespace() || value.is_control())
        {
            return Err(HarnessError::invalid("Worker token is invalid"));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| HarnessError::execution("create Worker HTTP client"))?;
        Ok(Self {
            http,
            origin,
            token: Arc::new(Zeroizing::new(token)),
            identity: Arc::new(RwLock::new(None)),
            lease_seconds: Arc::default(),
            lease_expires_at_ms: Arc::default(),
        })
    }

    pub async fn configuration(&self) -> Result<WorkerConfiguration, HarnessError> {
        self.json(self.request(reqwest::Method::GET, "configuration"))
            .await
    }

    pub async fn register(
        &self,
        request: WorkerRegisterRequest,
    ) -> Result<WorkerRegistration, HarnessError> {
        let registration: WorkerRegistration = self
            .json(
                self.request(reqwest::Method::POST, "register")
                    .json(&request),
            )
            .await?;
        *self
            .identity
            .write()
            .map_err(|_| HarnessError::execution("Worker identity lock is poisoned"))? =
            Some(registration.identity.clone());
        self.lease_seconds.store(
            registration.lease_seconds,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.record_heartbeat();
        Ok(registration)
    }

    pub(crate) fn worker_generation(&self) -> Result<u64, HarnessError> {
        Ok(self.current_identity()?.generation)
    }

    pub(crate) fn worker_identity(&self) -> Result<CloudWorkerIdentity, HarnessError> {
        self.current_identity()
    }

    pub async fn rpc(&self, request: WorkerRequest) -> Result<WorkerReply, HarnessError> {
        let identity = self.current_identity()?;
        self.json(
            self.request(reqwest::Method::POST, "rpc")
                .json(&WorkerRpcRequest { identity, request }),
        )
        .await
    }

    pub async fn heartbeat(&self) -> Result<(), HarnessError> {
        if let Err(error) = self.unit(WorkerRequest::Heartbeat).await {
            if error.code == ternilo_protocol::ErrorCode::PolicyDenied {
                self.lease_expires_at_ms
                    .store(0, std::sync::atomic::Ordering::Relaxed);
            }
            return Err(error);
        }
        self.record_heartbeat();
        Ok(())
    }

    pub fn lease_expires_at_ms(&self) -> u64 {
        self.lease_expires_at_ms
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn record_heartbeat(&self) {
        let duration = self
            .lease_seconds
            .load(std::sync::atomic::Ordering::Relaxed);
        if let Ok(now) = crate::now_ms() {
            self.lease_expires_at_ms.store(
                now.saturating_add(duration.saturating_mul(1_000)),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }

    pub async fn drain(&self) -> Result<u32, HarnessError> {
        match self.rpc(WorkerRequest::Drain).await? {
            WorkerReply::Count { count } => Ok(count),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn claim_run(&self) -> Result<Option<CloudRunClaim>, HarnessError> {
        match self.rpc(WorkerRequest::ClaimRun).await? {
            WorkerReply::Claim { claim } => Ok(claim),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn start_run(
        &self,
        claim: &CloudRunClaim,
    ) -> Result<Option<StartedRun>, HarnessError> {
        match self
            .rpc(WorkerRequest::StartRun { run: claim.into() })
            .await?
        {
            WorkerReply::Started { run } => Ok(run),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn release_claim(&self, claim: &CloudRunClaim) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::ReleaseClaim { run: claim.into() })
            .await
    }

    pub async fn workspace_recovery_candidates(
        &self,
        after: Option<ternilo_cloud::WorkspaceRecoveryCursor>,
    ) -> Result<ternilo_cloud::WorkspaceRecoveryPage, HarnessError> {
        match self
            .rpc(WorkerRequest::WorkspaceRecoveryCandidates { after })
            .await?
        {
            WorkerReply::WorkspaceRecovery { page } => Ok(page),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn confirm_workspace_recovery(
        &self,
        ticket: ternilo_cloud::WorkspaceRecoveryTicket,
    ) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::ConfirmWorkspaceRecovery { ticket })
            .await
    }

    pub async fn renew_run(&self, run: &StartedRun) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::RenewRun { run: run.into() }).await
    }

    pub async fn park_run(
        &self,
        run: &StartedRun,
        revision: u64,
        dependencies: Vec<AcceptedSubagentRun>,
    ) -> Result<u64, HarnessError> {
        match self
            .rpc(WorkerRequest::ParkRun {
                run: run.into(),
                activity_revision: revision,
                dependencies,
            })
            .await?
        {
            WorkerReply::Parked { revision } if revision > 0 => Ok(revision),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn resume_run(
        &self,
        run: &StartedRun,
        revision: u64,
        parked_revision: u64,
    ) -> Result<RunAdmission, HarnessError> {
        match self
            .rpc(WorkerRequest::ResumeRun {
                run: run.into(),
                activity_revision: revision,
                parked_revision,
            })
            .await?
        {
            WorkerReply::Resumed {
                admission: RunAdmission::Pending,
            } => Ok(RunAdmission::Pending),
            WorkerReply::Resumed {
                admission: RunAdmission::Ready { admission_epoch },
            } if admission_epoch > 0 => Ok(RunAdmission::Ready { admission_epoch }),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn release_resident(
        &self,
        run: &StartedRun,
        worker_generation: u64,
    ) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::ReleaseResident {
            run: run.into(),
            worker_generation,
        })
        .await
    }

    pub async fn cancel_requested(&self, run: &StartedRun) -> Result<bool, HarnessError> {
        match self
            .rpc(WorkerRequest::CancelRequested { run: run.into() })
            .await?
        {
            WorkerReply::Flag { value } => Ok(value),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn append_event(
        &self,
        run: &StartedRun,
        event: &SessionEvent,
    ) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::AppendEvent {
            run: run.into(),
            event: Box::new(event.clone()),
        })
        .await
    }

    pub async fn finish_run(
        &self,
        run: &StartedRun,
        terminal: TerminalState,
        outcome: Option<&RunOutcome>,
        error: Option<&HarnessError>,
    ) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::FinishRun {
            run: run.into(),
            terminal,
            outcome: outcome.cloned(),
            error: error.cloned(),
        })
        .await
    }

    pub async fn run_submission(
        &self,
        run: &StartedRun,
    ) -> Result<
        (
            Option<SessionSubmission>,
            Vec<ternilo_protocol::SteeringInput>,
        ),
        HarnessError,
    > {
        match self
            .rpc(WorkerRequest::RunSubmission { run: run.into() })
            .await?
        {
            WorkerReply::Submission {
                submission,
                additional_inputs,
            } => Ok((submission, additional_inputs)),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn record_question(
        &self,
        run: &StartedRun,
        question: &UserQuestion,
    ) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::RecordQuestion {
            run: run.into(),
            question: Box::new(question.clone()),
        })
        .await
    }

    pub async fn question_answer(
        &self,
        run: &StartedRun,
        question_id: &str,
    ) -> Result<Option<UserAnswer>, HarnessError> {
        match self
            .rpc(WorkerRequest::QuestionAnswer {
                run: run.into(),
                question_id: question_id.to_owned(),
            })
            .await?
        {
            WorkerReply::Answer { answer } => Ok(answer),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn extensions_for_run(
        &self,
        run: &StartedRun,
    ) -> Result<Vec<ternilo_extension::ExtensionDistribution>, HarnessError> {
        match self
            .rpc(WorkerRequest::Extensions { run: run.into() })
            .await?
        {
            WorkerReply::Extensions { extensions } => Ok(extensions),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn extensions_active(&self, run: &StartedRun) -> Result<bool, HarnessError> {
        match self
            .rpc(WorkerRequest::ExtensionsActive { run: run.into() })
            .await?
        {
            WorkerReply::Flag { value } => Ok(value),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn store_attachment(
        &self,
        run: &StartedRun,
        attachment: &Attachment,
        content: &[u8],
    ) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::StoreAttachment {
            run: run.into(),
            attachment: attachment.clone(),
            content_base64: STANDARD.encode(content),
        })
        .await
    }

    pub async fn download_attachment(
        &self,
        run: &StartedRun,
        attachment: &Attachment,
    ) -> Result<String, HarnessError> {
        match self
            .rpc(WorkerRequest::DownloadAttachment {
                run: run.into(),
                attachment: attachment.clone(),
            })
            .await?
        {
            WorkerReply::AttachmentObject { content_base64 } => Ok(content_base64),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn claim_commands(&self) -> Result<Vec<ClaimedCloudSessionCommand>, HarnessError> {
        match self.rpc(WorkerRequest::ClaimCommands).await? {
            WorkerReply::Commands { commands } => Ok(commands),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn complete_command(
        &self,
        command: &ClaimedCloudSessionCommand,
        reply: &CommandReply,
    ) -> Result<(), HarnessError> {
        if reply.command_id != command.command.command_id {
            return Err(HarnessError::invalid(
                "Worker reply does not match its command",
            ));
        }
        self.unit(WorkerRequest::CompleteCommand {
            command: command.into(),
            outcome: reply.outcome.clone(),
        })
        .await
    }

    pub async fn defer_command(
        &self,
        command: &ClaimedCloudSessionCommand,
    ) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::DeferCommand {
            command: command.into(),
        })
        .await
    }

    pub async fn steering_submission(
        &self,
        command: &ClaimedCloudSessionCommand,
    ) -> Result<Option<SessionSubmission>, HarnessError> {
        match self
            .rpc(WorkerRequest::SteeringSubmission {
                command: command.into(),
            })
            .await?
        {
            WorkerReply::Submission { submission, .. } => Ok(submission),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn complete_steering(
        &self,
        command: &ClaimedCloudSessionCommand,
        accepted: bool,
    ) -> Result<CommandReply, HarnessError> {
        match self
            .rpc(WorkerRequest::CompleteSteering {
                command: command.into(),
                accepted,
            })
            .await?
        {
            WorkerReply::Command { reply } => Ok(reply),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn requeue_steering(&self, run: &StartedRun) -> Result<u32, HarnessError> {
        match self
            .rpc(WorkerRequest::RequeueSteering { run: run.into() })
            .await?
        {
            WorkerReply::Count { count } => Ok(count),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn create_subagent(
        &self,
        run: &StartedRun,
        child_session_id: &SessionId,
        metadata: &SubagentSessionMetadata,
        label: &str,
    ) -> Result<SessionId, HarnessError> {
        match self
            .rpc(WorkerRequest::CreateSubagent {
                run: run.into(),
                child_session_id: child_session_id.clone(),
                metadata: metadata.clone(),
                label: label.to_owned(),
            })
            .await?
        {
            WorkerReply::SessionId { session_id } => Ok(session_id),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn enqueue_subagent(
        &self,
        run: &StartedRun,
        child_session_id: &SessionId,
        child_run_id: &RunId,
        input: &str,
    ) -> Result<AcceptedSubagentRun, HarnessError> {
        match self
            .rpc(WorkerRequest::EnqueueSubagent {
                run: run.into(),
                child_session_id: child_session_id.clone(),
                child_run_id: child_run_id.clone(),
                input: input.to_owned(),
            })
            .await?
        {
            WorkerReply::SubagentAccepted { run } => {
                run.validate()?;
                if run.session_id != *child_session_id || run.run_id != *child_run_id {
                    return Err(HarnessError::policy(
                        "Server accepted a different cloud Subagent run",
                    ));
                }
                Ok(run)
            }
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn subagent_run(
        &self,
        run: &StartedRun,
        child_session_id: &SessionId,
        child_run_id: &RunId,
    ) -> Result<Option<WorkerSubagentRun>, HarnessError> {
        match self
            .rpc(WorkerRequest::ReadSubagent {
                run: run.into(),
                child_session_id: child_session_id.clone(),
                child_run_id: child_run_id.clone(),
            })
            .await?
        {
            WorkerReply::Subagent { run } => Ok(run),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn cancel_subagent(
        &self,
        run: &StartedRun,
        child_session_id: &SessionId,
        child_run_id: &RunId,
    ) -> Result<CloudRunState, HarnessError> {
        match self
            .rpc(WorkerRequest::CancelSubagent {
                run: run.into(),
                child_session_id: child_session_id.clone(),
                child_run_id: child_run_id.clone(),
            })
            .await?
        {
            WorkerReply::RunState { state } => Ok(state),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn claim_telemetry(
        &self,
        lease: Duration,
    ) -> Result<Vec<ClaimedCloudTelemetry>, HarnessError> {
        match self
            .rpc(WorkerRequest::ClaimTelemetry {
                lease_ms: u64::try_from(lease.as_millis())
                    .map_err(|_| HarnessError::invalid("Worker telemetry lease is too long"))?,
            })
            .await?
        {
            WorkerReply::Telemetry { occurrences } => Ok(occurrences),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn acknowledge_telemetry(
        &self,
        occurrence: &ClaimedCloudTelemetry,
    ) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::AcknowledgeTelemetry {
            occurrence: telemetry_lease(occurrence),
        })
        .await
    }

    pub async fn fail_telemetry(
        &self,
        occurrence: &ClaimedCloudTelemetry,
        error: &str,
    ) -> Result<(), HarnessError> {
        self.unit(WorkerRequest::FailTelemetry {
            occurrence: telemetry_lease(occurrence),
            error: error.to_owned(),
        })
        .await
    }

    pub async fn inspection(
        &self,
        command: &ClaimedCloudSessionCommand,
    ) -> Result<WorkerInspection, HarnessError> {
        match self
            .rpc(WorkerRequest::Inspection {
                command: command.into(),
            })
            .await?
        {
            WorkerReply::Inspection {
                session,
                profile,
                extensions,
            } => Ok(WorkerInspection {
                session,
                profile,
                extensions,
            }),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn reference_contexts(
        &self,
        run: &StartedRun,
    ) -> Result<Vec<ReferenceContext>, HarnessError> {
        match self
            .rpc(WorkerRequest::ReferenceContexts { run: run.into() })
            .await?
        {
            WorkerReply::References { contexts } => Ok(contexts),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn agent_team(
        &self,
        run: &StartedRun,
        request: WorkerTeamRequest,
    ) -> Result<serde_json::Value, HarnessError> {
        match self
            .rpc(WorkerRequest::AgentTeam {
                run: run.into(),
                request,
            })
            .await?
        {
            WorkerReply::Team { value } => Ok(value),
            _ => Err(unexpected_reply()),
        }
    }

    pub async fn model_complete(
        &self,
        run: &StartedRun,
        request_id: u64,
        binding: &ternilo_protocol::RunModelBinding,
        request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Result<ModelResponse, HarnessError> {
        let request = WorkerModelRequest {
            identity: self.current_identity()?,
            run: run.into(),
            request_id,
            binding: binding.clone(),
            request,
        };
        // Dropping this future closes the single request; model work is never replayed here.
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(HarnessError::cancelled("Worker model request was cancelled")),
            result = self.stream_model(&request, output.as_ref()) => result,
        }
    }

    async fn stream_model(
        &self,
        request: &WorkerModelRequest,
        output: &dyn ModelOutput,
    ) -> Result<ModelResponse, HarnessError> {
        let response = self
            .request(reqwest::Method::POST, "model")
            .json(request)
            .send()
            .await
            .map_err(|_| model_interrupted())?;
        ternilo_builtins::read_model_gateway_response(response, output).await
    }

    fn current_identity(&self) -> Result<CloudWorkerIdentity, HarnessError> {
        self.identity
            .read()
            .map_err(|_| HarnessError::execution("Worker identity lock is poisoned"))?
            .clone()
            .ok_or_else(|| HarnessError::unavailable("Worker has not registered with its Server"))
    }

    fn request(&self, method: reqwest::Method, operation: &str) -> reqwest::RequestBuilder {
        let mut url = self.origin.clone();
        url.set_path(&format!("/internal/worker/v1/{operation}"));
        self.http
            .request(method, url)
            .bearer_auth(self.token.as_str())
    }

    async fn json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, HarnessError> {
        let response = request
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| HarnessError::unavailable("Worker Server request did not complete"))?;
        require_success(response)
            .await?
            .json()
            .await
            .map_err(|_| HarnessError::execution("Worker Server returned an invalid response"))
    }

    async fn unit(&self, request: WorkerRequest) -> Result<(), HarnessError> {
        match self.rpc(request).await? {
            WorkerReply::Unit => Ok(()),
            _ => Err(unexpected_reply()),
        }
    }
}

async fn require_success(response: reqwest::Response) -> Result<reqwest::Response, HarnessError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    match response.json::<ErrorResponse>().await {
        Ok(body) => Err(body.error),
        Err(_) => Err(HarnessError::execution(format!(
            "Worker Server returned HTTP {status}"
        ))),
    }
}

fn unexpected_reply() -> HarnessError {
    HarnessError::execution("Worker Server returned an unexpected reply type")
}

fn model_interrupted() -> HarnessError {
    HarnessError::execution(
        "Worker model connection ended without completion; its result is unknown and was not replayed",
    )
}

fn telemetry_lease(occurrence: &ClaimedCloudTelemetry) -> TelemetryLease {
    TelemetryLease {
        tenant_id: occurrence.identity.tenant_id.clone(),
        user_id: occurrence.identity.user_id.clone(),
        occurrence_id: occurrence.occurrence_id.clone(),
        attempt_count: occurrence.attempt_count,
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
