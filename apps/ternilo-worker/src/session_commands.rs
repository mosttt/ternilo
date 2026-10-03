use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use ternilo_cloud::{
    ClaimedCloudSessionCommand, CloudCommandDelivery, CloudWorkerIdentity, WorkerPolicy,
};
use ternilo_kernel::{HarnessSession, HostEnvironment, HostPolicy};
use ternilo_protocol::{
    HarnessError, PermissionPreset, SessionCommandCatalog, SessionIdentity, SessionMode,
    SkillCatalogSnapshot, WorkspaceBinding,
};
use ternilo_transport::{
    ApplicationOperation, CommandReply, EXECUTOR_PROTOCOL_VERSION, ExecutorCapability,
    ExecutorCommandBody, ExecutorHello, ExecutorId, ExecutorKind,
};
use tokio::sync::watch;

use crate::{
    client::{WorkerClient, WorkerInspection},
    now_ms,
    run::{ActiveRuns, prepare_steering_input},
    storage_root::RegisteredStorageRoot,
    workspace::existing_workspace,
};

pub(crate) fn worker_hello(worker_id: &str, catalog_revision: &str, now_ms: u64) -> ExecutorHello {
    ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id: ExecutorId::new(worker_id),
        executor_kind: ExecutorKind::CloudWorker,
        instance_nonce: format!("{}-{}-{now_ms}", worker_id, std::process::id()),
        catalog_revision: catalog_revision.to_owned(),
        capabilities: BTreeSet::from([
            ExecutorCapability::CloudRun,
            ExecutorCapability::AddressedSessionCommands,
            ExecutorCapability::SessionSteering,
            ExecutorCapability::RunCancellation,
            ExecutorCapability::Skills,
            ExecutorCapability::TelemetryDisclosure,
            ExecutorCapability::WorkspaceFiles,
        ]),
    }
}

#[derive(Clone)]
pub struct CommandPlane {
    identity: CloudWorkerIdentity,
    worker_lease_ttl: Duration,
    active_runs: ActiveRuns,
    workspace_root: RegisteredStorageRoot,
    policy: WorkerPolicy,
}

impl CommandPlane {
    pub fn new(
        identity: CloudWorkerIdentity,
        worker_lease_ttl: Duration,
        active_runs: ActiveRuns,
        workspace_root: RegisteredStorageRoot,
        policy: WorkerPolicy,
    ) -> Self {
        Self {
            identity,
            worker_lease_ttl,
            active_runs,
            workspace_root,
            policy,
        }
    }

    #[must_use]
    pub fn identity(&self) -> &CloudWorkerIdentity {
        &self.identity
    }

    pub async fn run(
        self,
        store: WorkerClient,
        poll_interval: Duration,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), HarnessError> {
        tokio::select! {
            result = self.drive(&store, poll_interval, &mut shutdown) => result,
            result = self.heartbeat(&store) => result,
        }
    }

    async fn heartbeat(&self, store: &WorkerClient) -> Result<(), HarnessError> {
        let heartbeat_period = self.worker_lease_ttl / 3;
        let mut heartbeat = tokio::time::interval_at(
            tokio::time::Instant::now() + heartbeat_period,
            heartbeat_period,
        );
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            heartbeat.tick().await;
            store.heartbeat().await?;
        }
    }

    async fn drive(
        &self,
        store: &WorkerClient,
        poll_interval: Duration,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), HarnessError> {
        let mut commands = tokio::time::interval(poll_interval);
        commands.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut starts = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                _ = commands.tick() => {
                    let claims = store.claim_commands().await?;
                    for claim in claims {
                        if matches!(claim.command.body, ExecutorCommandBody::Application { request: ApplicationOperation::SessionServiceStart { .. } }) {
                            let plane = self.clone();
                            let store = store.clone();
                            let (dispatched, sent) = tokio::sync::oneshot::channel();
                            starts.spawn(async move {
                                let ExecutorCommandBody::Application { request } = &claim.command.body else { unreachable!("service start is an application command") };
                                plane.dispatch_service(&store, &claim, request, Some(dispatched)).await
                            });
                            // Preserve wire order without waiting for service initialization.
                            let _ = sent.await;
                            continue;
                        }
                        if let Err(error) = self.dispatch_claim(store, &claim).await {
                            if fatal_command_plane_error(&error) {
                                return Err(error);
                            }
                            eprintln!(
                                "cloud Session command {} was not applied: {error}",
                                claim.command.command_id
                            );
                        }
                    }
                }
                result = starts.join_next(), if !starts.is_empty() => {
                    match result.expect("service start task exists") {
                        Ok(Ok(())) => {},
                        Ok(Err(error)) if fatal_command_plane_error(&error) => return Err(error),
                        Ok(Err(error)) => eprintln!("cloud service start was not applied: {error}"),
                        Err(error) => return Err(HarnessError::execution(format!("cloud service start task failed: {error}"))),
                    }
                }
            }
        }
    }

    async fn dispatch_claim(
        &self,
        store: &WorkerClient,
        claim: &ClaimedCloudSessionCommand,
    ) -> Result<(), HarnessError> {
        match &claim.command.body {
            ExecutorCommandBody::Application {
                request:
                    request @ (ApplicationOperation::SessionServices { .. }
                    | ApplicationOperation::SessionServiceStart { .. }
                    | ApplicationOperation::SessionServiceStop { .. }),
            } => self.dispatch_service(store, claim, request, None).await,
            ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionQueueSteer { .. },
            } => self.dispatch_steering(store, claim).await,
            ExecutorCommandBody::Application {
                request:
                    request @ (ApplicationOperation::SessionCommands { .. }
                    | ApplicationOperation::SessionSkills { .. }
                    | ApplicationOperation::SessionSkillResolve { .. }
                    | ApplicationOperation::SessionReferenceCandidates { .. }
                    | ApplicationOperation::SessionWorkspace { .. }),
            } => self.dispatch_inspection(store, claim, request).await,
            ExecutorCommandBody::CancelRun { run_id, .. } => {
                self.dispatch_cancel(store, claim, run_id).await
            }
            _ => {
                let completed_at_ms = now_ms()?;
                let reply = CommandReply::failure(
                    claim.command.command_id.clone(),
                    completed_at_ms,
                    HarnessError::policy(
                        "cloud Worker does not have a runtime handler for this Session command capability",
                    ),
                );
                store.complete_command(claim, &reply).await
            }
        }
    }

    async fn dispatch_service(
        &self,
        store: &WorkerClient,
        claim: &ClaimedCloudSessionCommand,
        request: &ApplicationOperation,
        dispatched: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<(), HarnessError> {
        let result = if let Some(handle) = self
            .active_runs
            .find_session(&claim.tenant_id, &claim.session_id)
        {
            if let CloudCommandDelivery::TargetRun {
                run_id,
                writer_fencing_token,
            } = &claim.delivery
                && !handle.matches_runtime(run_id, *writer_fencing_token)
            {
                Err(HarnessError::conflict(
                    "cloud service runtime changed before command delivery",
                ))
            } else {
                match request {
                    ApplicationOperation::SessionServices { .. } => match handle
                        .services(claim.command.command_id.clone(), Duration::from_secs(2))
                        .await
                    {
                        Ok(services) => serde_json::to_value(services)
                            .map_err(|error| HarnessError::execution(error.to_string())),
                        Err(_) if handle.commands_closed() => Ok(serde_json::json!([])),
                        Err(error) => Err(error),
                    },
                    ApplicationOperation::SessionServiceStart { service_id, .. }
                    | ApplicationOperation::SessionServiceStop { service_id, .. } => handle
                        .control_service(
                            claim.command.command_id.clone(),
                            service_id.clone(),
                            matches!(request, ApplicationOperation::SessionServiceStart { .. }),
                            Duration::from_secs(23),
                            dispatched,
                        )
                        .await
                        .and_then(|service| {
                            serde_json::to_value(service)
                                .map_err(|error| HarnessError::execution(error.to_string()))
                        }),
                    _ => unreachable!("service dispatcher received another operation"),
                }
            }
        } else if matches!(request, ApplicationOperation::SessionServices { .. }) {
            Ok(serde_json::json!([]))
        } else {
            Err(HarnessError::conflict(
                "cloud session has no active runtime to control",
            ))
        };
        let now = now_ms()?;
        let reply = match result {
            Ok(value) => CommandReply::success(claim.command.command_id.clone(), now, value),
            Err(error) => CommandReply::failure(claim.command.command_id.clone(), now, error),
        };
        store.complete_command(claim, &reply).await
    }

    async fn dispatch_inspection(
        &self,
        store: &WorkerClient,
        claim: &ClaimedCloudSessionCommand,
        request: &ApplicationOperation,
    ) -> Result<(), HarnessError> {
        let result = if matches!(
            request,
            ApplicationOperation::SessionReferenceCandidates { .. }
                | ApplicationOperation::SessionWorkspace { .. }
        ) {
            self.inspect_idle(store, claim, request).await
        } else if let Some(handle) = self
            .active_runs
            .find_session(&claim.tenant_id, &claim.session_id)
        {
            let result = self.inspect_active(&handle, claim, request).await;
            if result.is_err() && handle.commands_closed() {
                // Re-read only after this execution has ended, using the same authorized command.
                self.inspect_idle(store, claim, request).await
            } else {
                result
            }
        } else {
            self.inspect_idle(store, claim, request).await
        };
        let completed_at_ms = now_ms()?;
        let reply = match result {
            Ok(value) => {
                CommandReply::success(claim.command.command_id.clone(), completed_at_ms, value)
            }
            Err(error) => {
                CommandReply::failure(claim.command.command_id.clone(), completed_at_ms, error)
            }
        };
        store.complete_command(claim, &reply).await
    }

    async fn inspect_active(
        &self,
        handle: &crate::run::ActiveRunHandle,
        claim: &ClaimedCloudSessionCommand,
        request: &ApplicationOperation,
    ) -> Result<serde_json::Value, HarnessError> {
        let timeout = Duration::from_millis(
            claim
                .command
                .expires_at_ms
                .saturating_sub(now_ms()?)
                .min(10_000),
        );
        let value = match request {
            ApplicationOperation::SessionCommands { .. } => serde_json::to_value(
                handle
                    .command_catalog(claim.command.command_id.clone(), timeout)
                    .await?,
            ),
            ApplicationOperation::SessionSkills { .. } => serde_json::to_value(
                handle
                    .skill_catalog(claim.command.command_id.clone(), timeout)
                    .await?,
            ),
            ApplicationOperation::SessionSkillResolve { name, input, .. } => serde_json::to_value(
                handle
                    .resolve_skill(
                        claim.command.command_id.clone(),
                        name.clone(),
                        input.clone(),
                        timeout,
                    )
                    .await?,
            ),
            _ => unreachable!("inspection dispatcher received another command"),
        };
        value.map_err(|error| {
            HarnessError::execution(format!("encode cloud inspection reply: {error}"))
        })
    }

    async fn inspect_idle(
        &self,
        store: &WorkerClient,
        claim: &ClaimedCloudSessionCommand,
        request: &ApplicationOperation,
    ) -> Result<serde_json::Value, HarnessError> {
        let inspection = store.inspection(claim).await?;
        self.inspect_workspace(&inspection, request).await
    }

    async fn inspect_workspace(
        &self,
        inspection: &WorkerInspection,
        request: &ApplicationOperation,
    ) -> Result<serde_json::Value, HarnessError> {
        let session = &inspection.session;
        let workspace = existing_workspace(
            &self.workspace_root,
            &session.tenant_id,
            &session.workspace_id,
        )
        .await?;
        let command_directory;
        if workspace.is_none()
            && matches!(
                request,
                ApplicationOperation::SessionWorkspace {
                    request: ternilo_protocol::WorkspaceRequest::Info,
                    ..
                }
            )
        {
            return Ok(
                serde_json::json!({"root": session.workspace_id, "can_browse": true, "applications": []}),
            );
        }
        let workspace = match workspace {
            Some(workspace) => workspace,
            None if matches!(request, ApplicationOperation::SessionSkills { .. }) => {
                return serde_json::to_value(SkillCatalogSnapshot {
                    revision: 0,
                    complete: false,
                    skills: Vec::new(),
                })
                .map_err(|error| {
                    HarnessError::execution(format!("encode undiscovered skill catalog: {error}"))
                });
            }
            None if matches!(request, ApplicationOperation::SessionCommands { .. }) => {
                // Compose command metadata without initializing persistent files or preparing tools.
                command_directory = crate::private_temporary_directory(
                    "ternilo-command-inspection-",
                )
                .map_err(|error| {
                    HarnessError::execution(format!("create command inspection directory: {error}"))
                })?;
                command_directory.path().to_owned()
            }
            None => {
                return Err(HarnessError::invalid(
                    "Cloud workspace has not been initialized; start a task before browsing its files or skills",
                ));
            }
        };
        if let ApplicationOperation::SessionWorkspace { request, .. } = request {
            return ternilo_local::browse_workspace(workspace, request.clone()).await;
        }
        if let ApplicationOperation::SessionReferenceCandidates { request, .. } = request {
            let snapshot =
                ternilo_local::workspace_reference_candidates(&workspace, request).await?;
            return serde_json::to_value(snapshot).map_err(|error| {
                HarnessError::execution(format!("encode reference candidates: {error}"))
            });
        }
        let (harness, _extension_registry_directory) =
            self.boot_inspection_harness(inspection, workspace).await?;
        let result = inspect_harness(&harness, &session.session_id, &self.policy, request).await;
        let shutdown = harness.shutdown().await;
        match (result, shutdown) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    async fn boot_inspection_harness(
        &self,
        inspection: &WorkerInspection,
        workspace: PathBuf,
    ) -> Result<(HarnessSession, Option<tempfile::TempDir>), HarnessError> {
        let session = &inspection.session;
        let mut profile = inspection.profile.clone().ok_or_else(|| {
            HarnessError::execution("Worker inspection response has no execution profile")
        })?;
        let extensions = &inspection.extensions;
        let (catalog, extension_registry_directory) =
            crate::catalog_for_profile_extensions(&profile, extensions, &self.policy)?;
        self.policy
            .validate_profile_composition(&profile, &catalog)?;
        profile = ternilo_cloud::cloud_child_profile(profile);
        let permissions = if session.mode == SessionMode::Plan {
            PermissionPreset::ReadOnly
        } else {
            session.permissions
        };
        let workspace = WorkspaceBinding {
            workspace_id: session.workspace_id.clone(),
            path: workspace
                .to_str()
                .ok_or_else(|| HarnessError::execution("cloud workspace path is not UTF-8"))?
                .to_owned(),
        };
        let environment = HostEnvironment::memory(
            SessionIdentity {
                tenant_id: session.tenant_id.clone(),
                user_id: session.user_id.clone(),
                agent_id: session.agent_id.clone(),
                session_id: session.session_id.clone(),
            },
            Some(workspace),
            HostPolicy {
                limits: self.policy.maximum_limits,
                denied_tools: self.policy.denied_tools.clone(),
                permissions,
                allow_mutating_tools: permissions.allows_workspace_write(),
            },
        )
        .with_session_mode(session.mode);
        let harness = HarnessSession::boot(&catalog, &profile, environment).await?;
        Ok((harness, extension_registry_directory))
    }

    async fn dispatch_steering(
        &self,
        store: &WorkerClient,
        claim: &ClaimedCloudSessionCommand,
    ) -> Result<(), HarnessError> {
        let submission = store.steering_submission(claim).await?;
        let CloudCommandDelivery::TargetRun { run_id, .. } = &claim.delivery else {
            return Err(HarnessError::policy(
                "steering command is missing its active run target",
            ));
        };
        let Some(handle) = self
            .active_runs
            .wait_for(
                &claim.tenant_id,
                &claim.session_id,
                run_id,
                Duration::from_secs(1),
            )
            .await
        else {
            eprintln!(
                "cloud steering command {} was deferred because active run {} is not registered",
                claim.command.command_id, run_id
            );
            return store.defer_command(claim).await;
        };
        let accepted = if let Some(submission) = submission {
            match prepare_steering_input(&handle, claim.command.command_id.clone(), submission)
                .await
            {
                Ok(input) => match handle
                    .steer(
                        claim.command.command_id.clone(),
                        input,
                        Duration::from_secs(2),
                    )
                    .await
                {
                    Ok(true) => true,
                    Ok(false) => {
                        eprintln!(
                            "cloud steering command {} reached a closed child steering window",
                            claim.command.command_id
                        );
                        false
                    }
                    Err(error) => {
                        eprintln!(
                            "cloud steering command {} child delivery failed: {error}",
                            claim.command.command_id
                        );
                        false
                    }
                },
                Err(error) => {
                    eprintln!(
                        "cloud steering command {} input preparation failed: {error}",
                        claim.command.command_id
                    );
                    false
                }
            }
        } else {
            eprintln!(
                "cloud steering command {} no longer has a valid active submission",
                claim.command.command_id
            );
            false
        };
        store.complete_steering(claim, accepted).await.map(|_| ())
    }

    async fn dispatch_cancel(
        &self,
        store: &WorkerClient,
        claim: &ClaimedCloudSessionCommand,
        run_id: &ternilo_protocol::RunId,
    ) -> Result<(), HarnessError> {
        let Some(handle) = self
            .active_runs
            .wait_for(
                &claim.tenant_id,
                &claim.session_id,
                run_id,
                Duration::from_secs(1),
            )
            .await
        else {
            return store.defer_command(claim).await;
        };
        let result = handle
            .cancel(
                claim.command.command_id.clone(),
                run_id.clone(),
                Duration::from_secs(2),
            )
            .await;
        let completed_at_ms = now_ms()?;
        let reply = match result {
            Ok(()) => CommandReply::success(
                claim.command.command_id.clone(),
                completed_at_ms,
                serde_json::json!({ "cancelled": true }),
            ),
            Err(error) => {
                CommandReply::failure(claim.command.command_id.clone(), completed_at_ms, error)
            }
        };
        store.complete_command(claim, &reply).await
    }
}

async fn inspect_harness(
    harness: &HarnessSession,
    session_id: &ternilo_protocol::SessionId,
    policy: &WorkerPolicy,
    request: &ApplicationOperation,
) -> Result<serde_json::Value, HarnessError> {
    match request {
        ApplicationOperation::SessionCommands { .. } => {
            let commands = harness
                .command_catalog()
                .await?
                .into_iter()
                .filter(|command| !policy.denied_tools.contains(&command.tool_name))
                .map(|command| command.descriptor)
                .collect();
            serde_json::to_value(SessionCommandCatalog {
                session_id: session_id.clone(),
                commands,
            })
        }
        ApplicationOperation::SessionSkills { .. } => {
            serde_json::to_value(harness.skill_catalog().await?)
        }
        ApplicationOperation::SessionSkillResolve { name, input, .. } => {
            let skill = harness
                .skill(name.clone())
                .await?
                .ok_or_else(|| HarnessError::invalid(format!("unknown skill {name:?}")))?;
            serde_json::to_value(ternilo_builtins::prepare_skill_invocation(&skill, input)?)
        }
        _ => unreachable!("inspection harness received another command"),
    }
    .map_err(|error| HarnessError::execution(format!("encode inspection result: {error}")))
}

fn fatal_command_plane_error(error: &HarnessError) -> bool {
    error.message.contains("Worker identity lease")
        || error.message.contains("Worker generation")
        || error.message.contains("Worker credential")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::{
        AgentId, PreparedSkillInvocation, RunLimits, SessionId, TenantId, UserId, WorkspaceId,
    };

    fn inspection_policy() -> WorkerPolicy {
        WorkerPolicy {
            catalog_revision: ternilo_cloud::CLOUD_CATALOG_REVISION.to_owned(),
            policy_revision: "inspection-test".to_owned(),
            maximum_limits: RunLimits::default(),
            max_run_attempts: 1,
            max_tenant_workspace_bytes: 1_000_000,
            max_tenant_workspace_entries: 1_000,
            minimum_workspace_free_bytes: 0,
            allowed_plugin_kinds: BTreeSet::new(),
            max_extension_packages_per_run: 0,
            extension_host_policy: ternilo_extension::ExtensionHostPolicy::default(),
            denied_tools: BTreeSet::new(),
        }
    }

    fn empty_inspection() -> WorkerInspection {
        WorkerInspection {
            session: ternilo_cloud::CloudSessionRecord {
                tenant_id: TenantId::new("inspection-tenant"),
                session_id: SessionId::new("inspection-session"),
                user_id: UserId::new("inspection-user"),
                project_id: "inspection-project".to_owned(),
                workspace_id: WorkspaceId::new("inspection-workspace"),
                parent_session_id: None,
                subagent: None,
                agent_id: AgentId::new("inspection-agent"),
                title: "Inspect before first run".to_owned(),
                archived_at_ms: None,
                state: ternilo_cloud::CloudSessionState::Idle,
                execution: None,
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::default(),
                last_seq: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            profile: Some(ternilo_cloud::cloud_profile(None)),
            extensions: Vec::new(),
        }
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Verify cold metadata inspection and discovery after real workspace preparation in one lifecycle."
    )]
    async fn idle_inspection_keeps_commands_without_initializing_workspace_files() {
        let directory = tempfile::tempdir().unwrap();
        let root_id =
            crate::storage_root::load_or_create(directory.path(), "inspection-storage").unwrap();
        let root =
            RegisteredStorageRoot::open(directory.path(), "inspection-storage", &root_id).unwrap();
        let inspection = empty_inspection();
        let mut policy = inspection_policy();
        policy.allowed_plugin_kinds = inspection
            .profile
            .as_ref()
            .unwrap()
            .plugins
            .iter()
            .map(|plugin| plugin.kind.clone())
            .collect();
        let plane = CommandPlane::new(
            CloudWorkerIdentity {
                worker_id: ExecutorId::new("inspection-worker"),
                instance_nonce: "inspection-instance".to_owned(),
                generation: 1,
            },
            Duration::from_secs(30),
            ActiveRuns::default(),
            root.clone(),
            policy,
        );
        let session_id = inspection.session.session_id.clone();
        let workspace_info = plane
            .inspect_workspace(
                &inspection,
                &ApplicationOperation::SessionWorkspace {
                    session_id: session_id.clone(),
                    request: ternilo_protocol::WorkspaceRequest::Info,
                },
            )
            .await
            .unwrap();
        assert_eq!(workspace_info["can_browse"], true);
        assert_eq!(workspace_info["applications"], serde_json::json!([]));
        let commands: SessionCommandCatalog = serde_json::from_value(
            plane
                .inspect_workspace(
                    &inspection,
                    &ApplicationOperation::SessionCommands {
                        session_id: session_id.clone(),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(
            commands
                .commands
                .iter()
                .any(|command| command.name == "read")
        );
        let skills: SkillCatalogSnapshot = serde_json::from_value(
            plane
                .inspect_workspace(
                    &inspection,
                    &ApplicationOperation::SessionSkills {
                        session_id: session_id.clone(),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(skills.revision, 0);
        assert!(!skills.complete);
        assert!(skills.skills.is_empty());
        for request in [
            ApplicationOperation::SessionReferenceCandidates {
                session_id: session_id.clone(),
                request: ternilo_protocol::ReferenceCandidateRequest::default(),
            },
            ApplicationOperation::SessionSkillResolve {
                session_id: session_id.clone(),
                name: "release-check".to_owned(),
                input: String::new(),
            },
        ] {
            let error = plane
                .inspect_workspace(&inspection, &request)
                .await
                .unwrap_err();
            assert!(error.message.contains("has not been initialized"));
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

        let workspace = crate::workspace::prepare_workspace(
            &root,
            &inspection.session.tenant_id,
            &inspection.session.workspace_id,
            false,
        )
        .await
        .unwrap();
        std::fs::write(workspace.join("visible.txt"), "visible").unwrap();
        let without_profile = WorkerInspection {
            session: inspection.session.clone(),
            profile: None,
            extensions: Vec::new(),
        };
        let preview = plane
            .inspect_workspace(
                &without_profile,
                &ApplicationOperation::SessionWorkspace {
                    session_id: session_id.clone(),
                    request: ternilo_protocol::WorkspaceRequest::Read {
                        path: "visible.txt".to_owned(),
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(preview["content"], "visible");
        let skill_dir = workspace.join(".agents/skills/release-check");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: release-check\ndescription: Prepared workspace skill\nuser-invocable: true\n---\nCheck the prepared workspace.",
        )
        .unwrap();
        let discovered: SkillCatalogSnapshot = serde_json::from_value(
            plane
                .inspect_workspace(
                    &inspection,
                    &ApplicationOperation::SessionSkills {
                        session_id: session_id.clone(),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(discovered.complete);
        assert!(discovered.skills.iter().any(|skill| {
            skill.name == "release-check" && skill.description == "Prepared workspace skill"
        }));
        let candidates: ternilo_protocol::ReferenceCandidateSnapshot = serde_json::from_value(
            plane
                .inspect_workspace(
                    &inspection,
                    &ApplicationOperation::SessionReferenceCandidates {
                        session_id,
                        request: ternilo_protocol::ReferenceCandidateRequest::default(),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(candidates.candidates.iter().any(|candidate| matches!(candidate, ternilo_protocol::ReferenceCandidate::File { path, .. } if path == "visible.txt")));
    }

    async fn inspection_harness(workspace: &std::path::Path) -> HarnessSession {
        let session_id = SessionId::new("inspection-session");
        HarnessSession::boot(
            &ternilo_cloud::catalog().unwrap(),
            &ternilo_cloud::cloud_profile(None),
            HostEnvironment::memory(
                SessionIdentity {
                    tenant_id: TenantId::new("inspection-tenant"),
                    user_id: UserId::new("inspection-user"),
                    agent_id: AgentId::new("inspection-agent"),
                    session_id,
                },
                Some(WorkspaceBinding {
                    workspace_id: WorkspaceId::new("inspection-workspace"),
                    path: workspace.to_string_lossy().into_owned(),
                }),
                HostPolicy::local(RunLimits::default()),
            ),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Verify command, Skill and workspace refresh behavior in one real harness lifecycle."
    )]
    async fn workspace_inspection_refreshes_commands_skills_and_canonical_resolution() {
        let workspace = tempfile::tempdir().unwrap();
        let skill_dir = workspace.path().join(".agents/skills/release-check");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: release-check\ndescription: First description\nuser-invocable: true\n---\n# Release\nCheck version one.",
        )
        .await
        .unwrap();
        let private_dir = workspace.path().join(".agents/skills/private-review");
        tokio::fs::create_dir_all(&private_dir).await.unwrap();
        tokio::fs::write(
            private_dir.join("SKILL.md"),
            "---\nname: private-review\ndescription: Model-only review\nuser-invocable: false\n---\nKeep this private.",
        )
        .await
        .unwrap();
        let session_id = SessionId::new("inspection-session");
        let policy = inspection_policy();
        let harness = inspection_harness(workspace.path()).await;

        let commands: SessionCommandCatalog = serde_json::from_value(
            inspect_harness(
                &harness,
                &session_id,
                &policy,
                &ApplicationOperation::SessionCommands {
                    session_id: session_id.clone(),
                },
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert!(
            commands
                .commands
                .iter()
                .any(|command| command.name == "read")
        );
        let skills: SkillCatalogSnapshot = serde_json::from_value(
            inspect_harness(
                &harness,
                &session_id,
                &policy,
                &ApplicationOperation::SessionSkills {
                    session_id: session_id.clone(),
                },
            )
            .await
            .unwrap(),
        )
        .unwrap();
        let skill = skills
            .skills
            .iter()
            .find(|skill| skill.name == "release-check")
            .unwrap();
        assert_eq!(skill.description, "First description");
        assert_eq!(skill.provider, "filesystem");
        let invocation: PreparedSkillInvocation = serde_json::from_value(
            inspect_harness(
                &harness,
                &session_id,
                &policy,
                &ApplicationOperation::SessionSkillResolve {
                    session_id: session_id.clone(),
                    name: "release-check".to_owned(),
                    input: "ship it".to_owned(),
                },
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert!(invocation.model_input.contains("Check version one."));
        assert_eq!(invocation.display_input, "/skill release-check\n\nship it");
        let private = inspect_harness(
            &harness,
            &session_id,
            &policy,
            &ApplicationOperation::SessionSkillResolve {
                session_id: session_id.clone(),
                name: "private-review".to_owned(),
                input: String::new(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(private.code, ternilo_protocol::ErrorCode::PolicyDenied);
        let unknown = inspect_harness(
            &harness,
            &session_id,
            &policy,
            &ApplicationOperation::SessionSkillResolve {
                session_id: session_id.clone(),
                name: "missing-skill".to_owned(),
                input: String::new(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.code, ternilo_protocol::ErrorCode::InvalidInput);
        harness.shutdown().await.unwrap();

        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: release-check\ndescription: Updated description\nuser-invocable: true\n---\n# Release\nCheck version two.",
        )
        .await
        .unwrap();
        let refreshed = inspection_harness(workspace.path()).await;
        let skills: SkillCatalogSnapshot = serde_json::from_value(
            inspect_harness(
                &refreshed,
                &session_id,
                &policy,
                &ApplicationOperation::SessionSkills {
                    session_id: session_id.clone(),
                },
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert!(skills.skills.iter().any(|skill| {
            skill.name == "release-check"
                && skill.description == "Updated description"
                && skill.provider == "filesystem"
        }));
        refreshed.shutdown().await.unwrap();
    }
}
