use std::{
    collections::{BTreeMap, btree_map::Entry},
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

use futures_util::future::join_all;
use ternilo_cloud::StartedRun;
use ternilo_kernel::RunCancellation;
use ternilo_local::{fit_reference_contexts, resolve_file_references};
use ternilo_protocol::{
    HarnessError, PreparedSkillInvocation, RunId, SessionCommandCatalog, SessionId,
    SessionSubmission, SkillCatalogSnapshot, SteeringInput, SubmissionDelivery,
    SubmissionReference, TenantId, UserMessageSource,
};
use ternilo_transport::CommandId;
use tokio::sync::{Mutex, Notify, oneshot, watch};

use crate::child_protocol::{
    ChildSessionCommand, ChildSessionCommandOutcome, ParentProtocol, ParentToChildFrame,
};

#[derive(Clone, Default)]
pub struct ActiveRuns {
    inner: Arc<RwLock<BTreeMap<ActiveRunKey, Arc<ActiveRunHandle>>>>,
    changed: Arc<Notify>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[expect(
    clippy::struct_field_names,
    reason = "Keep canonical identity names consistent with the execution protocol."
)]
struct ActiveRunKey {
    tenant_id: TenantId,
    session_id: SessionId,
    run_id: RunId,
}

pub struct ActiveRunHandle {
    started: StartedRun,
    workspace: PathBuf,
    protocol: Arc<ParentProtocol>,
    model_cancellation: RunCancellation,
    durable_cancel: Arc<std::sync::atomic::AtomicBool>,
    pending: Mutex<BTreeMap<CommandId, oneshot::Sender<ChildSessionCommandOutcome>>>,
    commands_closed: watch::Sender<bool>,
}

impl ActiveRuns {
    pub fn register(
        &self,
        handle: Arc<ActiveRunHandle>,
    ) -> Result<ActiveRunRegistration, HarnessError> {
        let key = handle.key();
        {
            let mut active = self
                .inner
                .write()
                .expect("active run registry lock poisoned");
            reserve_active_run(&mut active, key.clone(), Arc::clone(&handle))?;
        }
        self.changed.notify_waiters();
        Ok(ActiveRunRegistration {
            registry: self.clone(),
            key,
            handle,
        })
    }

    pub fn find(
        &self,
        tenant_id: &TenantId,
        session_id: &SessionId,
        run_id: &RunId,
    ) -> Option<Arc<ActiveRunHandle>> {
        self.inner
            .read()
            .expect("active run registry lock poisoned")
            .get(&ActiveRunKey {
                tenant_id: tenant_id.clone(),
                session_id: session_id.clone(),
                run_id: run_id.clone(),
            })
            .cloned()
    }

    pub fn find_session(
        &self,
        tenant_id: &TenantId,
        session_id: &SessionId,
    ) -> Option<Arc<ActiveRunHandle>> {
        self.inner
            .read()
            .expect("active run registry lock poisoned")
            .iter()
            .find(|(key, _)| &key.tenant_id == tenant_id && &key.session_id == session_id)
            .map(|(_, handle)| Arc::clone(handle))
    }

    pub async fn wait_for(
        &self,
        tenant_id: &TenantId,
        session_id: &SessionId,
        run_id: &RunId,
        timeout: Duration,
    ) -> Option<Arc<ActiveRunHandle>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(handle) = self.find(tenant_id, session_id, run_id) {
                return Some(handle);
            }
            let changed = self.changed.notified();
            if let Some(handle) = self.find(tenant_id, session_id, run_id) {
                return Some(handle);
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return None;
            }
        }
    }

    /// Ask every active child to stop before the daemon drains. If a child does not respond,
    /// the caller may still abort its supervisor, which intentionally leaves physical occupancy
    /// unconfirmed for the next generation to recover.
    pub async fn request_shutdown(&self, timeout: Duration) {
        let handles = self
            .inner
            .read()
            .expect("active run registry lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        join_all(handles.into_iter().map(|handle| async move {
            let command = CommandId::new(format!(
                "worker-shutdown-{}",
                handle.started.claim.run_id.as_str()
            ));
            let _ = handle
                .cancel(command, handle.started.claim.run_id.clone(), timeout)
                .await;
        }))
        .await;
    }
}

pub struct ActiveRunRegistration {
    registry: ActiveRuns,
    key: ActiveRunKey,
    handle: Arc<ActiveRunHandle>,
}

fn reserve_active_run<T>(
    active: &mut BTreeMap<ActiveRunKey, Arc<T>>,
    key: ActiveRunKey,
    handle: Arc<T>,
) -> Result<(), HarnessError> {
    match active.entry(key) {
        Entry::Vacant(entry) => {
            entry.insert(handle);
            Ok(())
        }
        Entry::Occupied(_) => Err(HarnessError::conflict(
            "cloud Worker already has this run registered",
        )),
    }
}

impl Drop for ActiveRunRegistration {
    fn drop(&mut self) {
        let mut active = self
            .registry
            .inner
            .write()
            .expect("active run registry lock poisoned");
        if active
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.handle))
        {
            active.remove(&self.key);
            self.handle.commands_closed.send_replace(true);
        }
    }
}

impl ActiveRunHandle {
    #[must_use]
    pub fn new(
        started: StartedRun,
        workspace: PathBuf,
        protocol: Arc<ParentProtocol>,
        model_cancellation: RunCancellation,
        durable_cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            started,
            workspace,
            protocol,
            model_cancellation,
            durable_cancel,
            pending: Mutex::new(BTreeMap::new()),
            commands_closed: watch::channel(false).0,
        }
    }

    #[must_use]
    pub fn workspace(&self) -> &std::path::Path {
        &self.workspace
    }

    pub fn matches_runtime(&self, run_id: &RunId, fencing_token: u64) -> bool {
        self.started.claim.run_id == *run_id && self.started.fencing_token == fencing_token
    }

    pub async fn steer(
        &self,
        command_id: CommandId,
        input: SteeringInput,
        timeout: Duration,
    ) -> Result<bool, HarnessError> {
        match self
            .request(
                command_id,
                ChildSessionCommand::Steer {
                    input: Box::new(input),
                },
                timeout,
            )
            .await?
        {
            ChildSessionCommandOutcome::Steer { accepted } => Ok(accepted),
            ChildSessionCommandOutcome::Error { error } => Err(error),
            ChildSessionCommandOutcome::Cancelled
            | ChildSessionCommandOutcome::Commands { .. }
            | ChildSessionCommandOutcome::Skills { .. }
            | ChildSessionCommandOutcome::Services { .. }
            | ChildSessionCommandOutcome::Service { .. }
            | ChildSessionCommandOutcome::SkillResolved { .. } => Err(HarnessError::execution(
                "cloud child returned the wrong steering reply",
            )),
        }
    }

    pub async fn cancel(
        &self,
        command_id: CommandId,
        run_id: RunId,
        timeout: Duration,
    ) -> Result<(), HarnessError> {
        self.durable_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        self.model_cancellation.cancel();
        match self
            .request(command_id, ChildSessionCommand::Cancel { run_id }, timeout)
            .await?
        {
            ChildSessionCommandOutcome::Cancelled => Ok(()),
            ChildSessionCommandOutcome::Error { error } => Err(error),
            ChildSessionCommandOutcome::Steer { .. }
            | ChildSessionCommandOutcome::Commands { .. }
            | ChildSessionCommandOutcome::Skills { .. }
            | ChildSessionCommandOutcome::Services { .. }
            | ChildSessionCommandOutcome::Service { .. }
            | ChildSessionCommandOutcome::SkillResolved { .. } => Err(HarnessError::execution(
                "cloud child returned the wrong Session command reply",
            )),
        }
    }

    pub async fn command_catalog(
        &self,
        command_id: CommandId,
        timeout: Duration,
    ) -> Result<SessionCommandCatalog, HarnessError> {
        match self
            .request(command_id, ChildSessionCommand::Commands, timeout)
            .await?
        {
            ChildSessionCommandOutcome::Commands { catalog } => Ok(catalog),
            ChildSessionCommandOutcome::Error { error } => Err(error),
            _ => Err(HarnessError::execution(
                "cloud child returned the wrong command catalog reply",
            )),
        }
    }

    pub async fn skill_catalog(
        &self,
        command_id: CommandId,
        timeout: Duration,
    ) -> Result<SkillCatalogSnapshot, HarnessError> {
        match self
            .request(command_id, ChildSessionCommand::Skills, timeout)
            .await?
        {
            ChildSessionCommandOutcome::Skills { catalog } => Ok(catalog),
            ChildSessionCommandOutcome::Error { error } => Err(error),
            _ => Err(HarnessError::execution(
                "cloud child returned the wrong Skill catalog reply",
            )),
        }
    }

    pub async fn resolve_skill(
        &self,
        command_id: CommandId,
        name: String,
        input: String,
        timeout: Duration,
    ) -> Result<PreparedSkillInvocation, HarnessError> {
        match self
            .request(
                command_id,
                ChildSessionCommand::ResolveSkill { name, input },
                timeout,
            )
            .await?
        {
            ChildSessionCommandOutcome::SkillResolved { invocation } => Ok(invocation),
            ChildSessionCommandOutcome::Error { error } => Err(error),
            _ => Err(HarnessError::execution(
                "cloud child returned the wrong Skill resolution reply",
            )),
        }
    }

    pub async fn resolve_command(
        &self,
        command_id: &CommandId,
        outcome: ChildSessionCommandOutcome,
    ) {
        if let Some(sender) = self.pending.lock().await.remove(command_id) {
            let _ = sender.send(outcome);
        }
    }

    #[must_use]
    pub fn durable_cancel_requested(&self) -> bool {
        self.durable_cancel
            .load(std::sync::atomic::Ordering::Acquire)
    }

    #[must_use]
    pub fn commands_closed(&self) -> bool {
        *self.commands_closed.borrow()
    }

    pub async fn services(
        &self,
        command_id: CommandId,
        timeout: Duration,
    ) -> Result<Vec<ternilo_protocol::SessionServiceSnapshot>, HarnessError> {
        match self
            .request(command_id, ChildSessionCommand::Services, timeout)
            .await?
        {
            ChildSessionCommandOutcome::Services { services } => Ok(services),
            ChildSessionCommandOutcome::Error { error } => Err(error),
            _ => Err(HarnessError::execution(
                "cloud child returned the wrong services reply",
            )),
        }
    }

    pub async fn control_service(
        &self,
        command_id: CommandId,
        service_id: String,
        start: bool,
        timeout: Duration,
        dispatched: Option<oneshot::Sender<()>>,
    ) -> Result<ternilo_protocol::SessionServiceSnapshot, HarnessError> {
        let command = if start {
            ChildSessionCommand::StartService { service_id }
        } else {
            ChildSessionCommand::StopService { service_id }
        };
        match self
            .request_dispatched(command_id, command, timeout, dispatched)
            .await?
        {
            ChildSessionCommandOutcome::Service { service } => Ok(service),
            ChildSessionCommandOutcome::Error { error } => Err(error),
            _ => Err(HarnessError::execution(
                "cloud child returned the wrong service-control reply",
            )),
        }
    }

    async fn request(
        &self,
        command_id: CommandId,
        command: ChildSessionCommand,
        timeout: Duration,
    ) -> Result<ChildSessionCommandOutcome, HarnessError> {
        self.request_dispatched(command_id, command, timeout, None)
            .await
    }

    async fn request_dispatched(
        &self,
        command_id: CommandId,
        command: ChildSessionCommand,
        timeout: Duration,
        dispatched: Option<oneshot::Sender<()>>,
    ) -> Result<ChildSessionCommandOutcome, HarnessError> {
        let requires_live_runtime = matches!(
            command,
            ChildSessionCommand::Commands
                | ChildSessionCommand::Services
                | ChildSessionCommand::StartService { .. }
                | ChildSessionCommand::StopService { .. }
                | ChildSessionCommand::Skills
                | ChildSessionCommand::ResolveSkill { .. }
        );
        let mut closed = self.commands_closed.subscribe();
        if requires_live_runtime && *closed.borrow() {
            return Err(closed_command_error());
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            reserve_pending_command(&mut pending, command_id.clone(), sender)?;
        }
        if let Err(error) = self
            .protocol
            .send(&ParentToChildFrame::SessionCommand {
                command_id: command_id.clone(),
                command,
            })
            .await
        {
            self.pending.lock().await.remove(&command_id);
            return Err(if requires_live_runtime && self.commands_closed() {
                closed_command_error()
            } else {
                error
            });
        }
        if let Some(dispatched) = dispatched {
            let _ = dispatched.send(());
        }
        let completion = async {
            match tokio::time::timeout(timeout, receiver).await {
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(_)) => Err(closed_command_error()),
                Err(_) => Err(HarnessError::unavailable(
                    "cloud child Session command timed out",
                )),
            }
        };
        let result = tokio::select! {
            biased;
            result = completion => result,
            _ = closed.changed(), if requires_live_runtime => Err(closed_command_error()),
        };
        self.pending.lock().await.remove(&command_id);
        result
    }

    fn key(&self) -> ActiveRunKey {
        ActiveRunKey {
            tenant_id: self.started.claim.tenant_id.clone(),
            session_id: self.started.claim.session_id.clone(),
            run_id: self.started.claim.run_id.clone(),
        }
    }
}

fn reserve_pending_command(
    pending: &mut BTreeMap<CommandId, oneshot::Sender<ChildSessionCommandOutcome>>,
    command_id: CommandId,
    sender: oneshot::Sender<ChildSessionCommandOutcome>,
) -> Result<(), HarnessError> {
    match pending.entry(command_id) {
        Entry::Vacant(entry) => {
            entry.insert(sender);
            Ok(())
        }
        Entry::Occupied(_) => Err(HarnessError::conflict(
            "cloud child command is already pending",
        )),
    }
}

fn closed_command_error() -> HarnessError {
    HarnessError::unavailable("cloud child closed before replying to a Session command")
}

pub async fn prepare_steering_input(
    handle: &ActiveRunHandle,
    command_id: CommandId,
    submission: SessionSubmission,
) -> Result<SteeringInput, HarnessError> {
    if submission.content.regeneration_target().is_some() {
        return Err(HarnessError::invalid(
            "regeneration cannot be injected into an active turn",
        ));
    }
    let (input, display_input, skill_name) = match submission.content.skill_name() {
        None => (submission.content.input().to_owned(), None, None),
        Some(name) => {
            let prepared = handle
                .resolve_skill(
                    command_id,
                    name.to_owned(),
                    submission.content.input().to_owned(),
                    Duration::from_secs(2),
                )
                .await?;
            (
                prepared.model_input,
                Some(prepared.display_input),
                Some(prepared.name),
            )
        }
    };
    if submission
        .references
        .iter()
        .any(|reference| matches!(reference, SubmissionReference::Session { .. }))
    {
        return Err(HarnessError::policy(
            "cloud Session-reference steering requires a Worker reference snapshot",
        ));
    }
    let reference_contexts = fit_reference_contexts(
        resolve_file_references(handle.workspace(), &submission.references).await?,
    );
    let source = UserMessageSource::Submission {
        regenerate_from: None,
        submission_id: submission.id.clone(),
        created_at_ms: submission.created_at_ms,
        delivery: SubmissionDelivery::Steer,
        skill_name,
    };
    let input = SteeringInput {
        provenance: submission.provenance,
        submission_id: submission.id,
        input,
        display_input,
        source,
        references: submission.references,
        reference_contexts,
        attachments: submission.attachments,
    };
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started_run() -> StartedRun {
        let metadata = ternilo_protocol::RunMetadata {
            tenant_id: TenantId::new("tenant"),
            user_id: ternilo_protocol::UserId::new("user"),
            project_id: Some("project".to_owned()),
            workspace_id: ternilo_protocol::WorkspaceId::new("workspace"),
            agent_id: ternilo_protocol::AgentId::new("agent"),
            session_id: SessionId::new("session"),
            run_id: RunId::new("run"),
        };
        StartedRun {
            claim: ternilo_cloud::CloudRunClaim {
                provenance: None,
                tenant_id: metadata.tenant_id.clone(),
                actor_user_id: metadata.user_id.clone(),
                authorization_session_id: metadata.session_id.clone(),
                run_id: metadata.run_id.clone(),
                session_id: metadata.session_id.clone(),
                workspace_use: ternilo_cloud::WorkspaceUseTicket {
                    storage_id: "test-storage".to_owned(),
                    root_id: "test-root".to_owned(),
                    tenant_id: metadata.tenant_id.clone(),
                    workspace_id: metadata.workspace_id.clone(),
                    family_id: "test-family".to_owned(),
                    worker_id: "test-worker".to_owned(),
                    worker_generation: 1,
                    occupation_epoch: 1,
                    run_id: metadata.run_id.clone(),
                    lease_token: 1,
                },
                lease_token: 1,
                spec: ternilo_protocol::RunSpec {
                    schema_version: ternilo_protocol::RUN_SPEC_VERSION,
                    catalog_revision: "catalog".to_owned(),
                    policy_revision: "policy".to_owned(),
                    metadata,
                    limits: ternilo_protocol::RunLimits::default(),
                    permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
                    mode: ternilo_protocol::SessionMode::Execute,
                    profile: ternilo_protocol::Profile::default(),
                    input: "test".to_owned(),
                    references: Vec::new(),
                    reference_contexts: Vec::new(),
                    attachments: Vec::new(),
                },
                spec_digest: [0; 32],
            },
            fencing_token: 1,
            prior_events: Vec::new(),
        }
    }

    #[tokio::test]
    async fn child_session_command_timeout_is_unavailable() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let protocol = Arc::new(ParentProtocol::new(child.stdin.take().unwrap()));
        let handle = ActiveRunHandle::new(
            started_run(),
            PathBuf::from("/workspace"),
            protocol,
            RunCancellation::new(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );

        let error = handle
            .command_catalog(CommandId::new("timeout"), Duration::from_millis(10))
            .await
            .unwrap_err();

        assert_eq!(error.code, ternilo_protocol::ErrorCode::Unavailable);
        child.kill().await.unwrap();
    }

    #[tokio::test]
    async fn ending_an_execution_wakes_only_read_only_commands_without_waiting_for_timeout() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let handle = Arc::new(ActiveRunHandle::new(
            started_run(),
            PathBuf::from("/workspace"),
            Arc::new(ParentProtocol::new(child.stdin.take().unwrap())),
            RunCancellation::new(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));
        let registry = ActiveRuns::default();
        let registration = registry.register(Arc::clone(&handle)).unwrap();
        let reader = Arc::clone(&handle);
        let read = tokio::spawn(async move {
            reader
                .skill_catalog(CommandId::new("ending-inspection"), Duration::from_secs(60))
                .await
        });
        let writer = Arc::clone(&handle);
        let cancel_id = CommandId::new("ending-cancel");
        let cancel_request_id = cancel_id.clone();
        let cancel = tokio::spawn(async move {
            writer
                .request(
                    cancel_request_id,
                    ChildSessionCommand::Cancel {
                        run_id: RunId::new("run"),
                    },
                    Duration::from_secs(60),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while handle.pending.lock().await.len() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(registration);
        let error = tokio::time::timeout(Duration::from_secs(1), read)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, ternilo_protocol::ErrorCode::Unavailable);
        assert!(error.message.contains("closed before replying"));
        assert!(handle.commands_closed());
        assert!(
            !cancel.is_finished(),
            "ending a run must not invent the result of a cancellation command"
        );
        handle
            .resolve_command(&cancel_id, ChildSessionCommandOutcome::Cancelled)
            .await;
        assert_eq!(
            cancel.await.unwrap().unwrap(),
            ChildSessionCommandOutcome::Cancelled
        );
        assert!(handle.pending.lock().await.is_empty());
        let error = handle
            .command_catalog(CommandId::new("closed-inspection"), Duration::from_secs(60))
            .await
            .unwrap_err();
        assert_eq!(error.code, ternilo_protocol::ErrorCode::Unavailable);
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }

    #[test]
    fn duplicate_command_does_not_replace_the_first_waiter() {
        let command_id = CommandId::new("duplicate");
        let mut pending = BTreeMap::new();
        let (first_sender, mut first_receiver) = oneshot::channel();
        reserve_pending_command(&mut pending, command_id.clone(), first_sender).unwrap();
        let (duplicate_sender, _duplicate_receiver) = oneshot::channel();
        let error = reserve_pending_command(&mut pending, command_id.clone(), duplicate_sender)
            .expect_err("duplicate command must be rejected");
        assert_eq!(error.code, ternilo_protocol::ErrorCode::Conflict);
        assert!(matches!(
            first_receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        pending
            .remove(&command_id)
            .expect("first waiter remains registered")
            .send(ChildSessionCommandOutcome::Steer { accepted: true })
            .unwrap();
        assert_eq!(
            first_receiver.try_recv().unwrap(),
            ChildSessionCommandOutcome::Steer { accepted: true }
        );
    }

    #[test]
    fn duplicate_run_registration_does_not_replace_the_first_handle() {
        let key = ActiveRunKey {
            tenant_id: TenantId::new("tenant"),
            session_id: SessionId::new("session"),
            run_id: RunId::new("run"),
        };
        let first = Arc::new(1_u8);
        let duplicate = Arc::new(2_u8);
        let mut active = BTreeMap::new();
        reserve_active_run(&mut active, key.clone(), Arc::clone(&first)).unwrap();
        let error = reserve_active_run(&mut active, key.clone(), duplicate)
            .expect_err("duplicate run must be rejected");
        assert_eq!(error.code, ternilo_protocol::ErrorCode::Conflict);
        assert!(Arc::ptr_eq(active.get(&key).unwrap(), &first));
    }
}
