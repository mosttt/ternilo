use super::{
    AgentTeamProvider, Arc, Catalog, HarnessError, HarnessSession, HostEnvironment,
    JsonlEventStore, LocalAgentTeamProvider, LocalApplication, LocalInputReferences,
    ManagedSession, ModelSelection, Mutex, PathBuf, PermissionPreset, PluginEntry, Profile,
    ProviderProfile, ProviderProtocol, SessionMode, WorkspaceBinding, compose_profiles,
    validate_profile,
};
use ternilo_protocol::ProviderModelCatalog as _;

impl LocalApplication {
    pub async fn shutdown(&self) -> Result<(), HarnessError> {
        self.stopping.cancel();
        self.authorizations.shutdown().await;
        let sessions = {
            let mut live = self.live.write().await;
            std::mem::take(&mut *live).into_iter().collect::<Vec<_>>()
        };
        let mut failures = Vec::new();
        for (session_id, managed) in &sessions {
            if let Err(error) = self.inbox.pause(session_id).await {
                failures.push(error.to_string());
            }
            if let Some(run_id) = managed.harness.active_run().await
                && let Err(error) = managed.harness.cancel(run_id).await
            {
                failures.push(error.to_string());
            }
            self.interaction.cancel_session(session_id).await;
        }
        for (session_id, managed) in sessions {
            let gate = Arc::clone(&managed.gate).lock_owned();
            tokio::pin!(gate);
            let _gate = loop {
                tokio::select! {
                    gate = &mut gate => break gate,
                    () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        // A turn may have passed the host stop check while its
                        // agent is still preparing to register the active run.
                        if let Some(run_id) = managed.harness.active_run().await {
                            let _ = managed.harness.cancel(run_id).await;
                        }
                        self.interaction.cancel_session(&session_id).await;
                    }
                }
            };
            self.interaction.cancel_session(&session_id).await;
            self.checkpoint_session_fail_soft(&session_id).await;
            if let Err(error) = managed.harness.shutdown().await {
                failures.push(error.to_string());
            }
        }
        let mut tasks =
            std::mem::take(&mut *self.submission_tasks.lock().expect("submission task lock"));
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
        // Await SQLite worker closure before callers release or move the data directory.
        for result in [
            self.session_archive.close().await,
            self.agent_team.close().await,
            self.inbox.close().await,
        ] {
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(HarnessError::execution(format!(
                "local session shutdown failed: {}",
                failures.join("; ")
            )))
        }
    }

    pub(super) async fn session_lifecycle(&self, id: &str) -> Arc<Mutex<()>> {
        let mut sessions = self.session_lifecycle.lock().await;
        Arc::clone(
            sessions
                .entry(id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    pub(super) async fn addressable_session(
        &self,
        id: &str,
    ) -> Result<Arc<ManagedSession>, HarnessError> {
        if let Some(managed) = self.live.read().await.get(id).cloned() {
            return Ok(managed);
        }
        let lifecycle = self.session_lifecycle(id).await;
        let _lifecycle = lifecycle.lock().await;
        self.ensure_session_locked(id).await
    }

    pub(super) async fn stop_live_session(&self, id: &str) -> Result<(), HarnessError> {
        // A run can hold the turn gate while waiting for a question or tool
        // approval. Close that waiter before joining the runtime so profile
        // changes, plugin unloads, archive, and delete cannot deadlock.
        self.interaction.cancel_session(id).await;
        self.invalidate(Some(id), crate::LocalInvalidationCategory::Questions, None);
        let managed = self.live.write().await.remove(id);
        if let Some(managed) = managed {
            let _gate = Arc::clone(&managed.gate).lock_owned().await;
            self.checkpoint_session_fail_soft(id).await;
            managed.harness.shutdown().await?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "assemble trusted session services before publishing its runtime"
    )]
    pub(super) async fn ensure_session_locked(
        &self,
        id: &str,
    ) -> Result<Arc<ManagedSession>, HarnessError> {
        self.stopping.check()?;
        if let Some(managed) = self.live.read().await.get(id).cloned() {
            return Ok(managed);
        }
        let session = self
            .state
            .session(id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        let store = Arc::new(
            JsonlEventStore::new(&self.state.sessions_dir(), &session.identity.session_id)
                .with_index(
                    self.session_archive.index(),
                    &session.identity.session_id,
                    &session.workspace_id,
                )
                .with_notifications(
                    self.event_notifications.clone(),
                    &session.identity.session_id,
                )
                .with_execution_activity(
                    Arc::clone(&self.execution_activity),
                    &session.identity.session_id,
                ),
        );
        let mut policy = self.policy.clone();
        if session.mode == SessionMode::Plan {
            policy.permissions = PermissionPreset::ReadOnly;
            policy.allow_mutating_tools = false;
        } else {
            policy.permissions = session.permissions;
        }
        let interaction = self
            .interaction
            .for_session(session.identity.session_id.clone());
        let named_provider = resolve_named_provider(&session.model, &self.providers).await?;
        let profile = session_profile(
            &self.profile,
            &session.model,
            session.server_model.as_ref(),
            named_provider.as_ref(),
            &session.preset_plugins,
            &session.profile_plugins,
            session.mode,
        );
        let secrets: Arc<dyn ternilo_kernel::SecretResolver> = self.credentials.clone();
        let session_archive: Arc<dyn ternilo_kernel::SessionArchive> = self.session_archive.clone();
        let attachments: Arc<dyn ternilo_kernel::AttachmentResolver> = self.attachments.clone();
        let runtime_extensions: Arc<dyn ternilo_kernel::RuntimeExtensionsProvider> =
            self.runtime_extensions.clone();
        let agent_team: Arc<dyn AgentTeamProvider> = Arc::new(LocalAgentTeamProvider::new(
            Arc::clone(&self.agent_team),
            Arc::clone(&self.state),
            session.identity.session_id.clone(),
            self.invalidations.clone(),
        ));
        let telemetry = Arc::new(ternilo_kernel::HostSessionTelemetry::default());
        let execution = super::directory_admission::SessionDirectoryExecution::bind(
            self.directory_coordinator.clone(),
            Arc::clone(&self.state),
            Arc::downgrade(&self.live),
            Arc::clone(&self.directory_account_owner),
            session.identity.session_id.as_str().to_owned(),
            self.inbox
                .execution_scope(session.identity.session_id.as_str())
                .await?,
            PathBuf::from(&session.workspace_path),
        );
        let input_references = Arc::new(LocalInputReferences::new(
            Arc::clone(&self.state),
            session.identity.session_id.clone(),
        ));
        let environment = HostEnvironment::with_interaction_and_secrets(
            session.identity,
            Some(WorkspaceBinding {
                workspace_id: session.workspace_id,
                path: session.workspace_path,
            }),
            policy,
            store,
            interaction,
            secrets,
        )
        .with_model_gateway(self.server_models.for_session(id.to_owned()))
        .with_session_mode(session.mode)
        .with_workspace_execution(execution)
        .with_input_reference_resolver(input_references)
        .with_attachment_resolver(attachments)
        .with_session_archive(session_archive)
        .with_runtime_extensions(runtime_extensions)
        .with_subagent_sessions(self.subagent_session_host())
        .with_agent_team(agent_team)
        .with_session_telemetry(Arc::clone(&telemetry));
        let harness = HarnessSession::boot(&self.catalog, &profile, environment).await?;
        let created = Arc::new(ManagedSession {
            harness,
            gate: Arc::new(Mutex::new(())),
            telemetry,
            boot_mode: session.mode,
        });
        let mut live = self.live.write().await;
        if self.stopping.is_cancelled() {
            drop(live);
            created.harness.shutdown().await?;
            return Err(HarnessError::cancelled(
                "local application is shutting down",
            ));
        }
        if let Some(existing) = live.get(id).cloned() {
            drop(live);
            created.harness.shutdown().await?;
            Ok(existing)
        } else {
            live.insert(id.to_owned(), Arc::clone(&created));
            Ok(created)
        }
    }
}

pub(super) fn validate_local_profile(
    profile: &Profile,
    catalog: &Catalog,
    extension_registry: &ternilo_extension::ExtensionRegistry,
) -> Result<(), HarnessError> {
    validate_profile(profile, catalog)?;
    extension_registry.validate_profile_mounts(profile)
}

pub(super) async fn resolve_named_provider(
    selection: &ModelSelection,
    providers: &crate::LocalProviders,
) -> Result<Option<ProviderProfile>, HarnessError> {
    let ModelSelection::NamedProvider {
        provider_id,
        model,
        reasoning_effort,
    } = selection
    else {
        return Ok(None);
    };
    let provider = providers.get(provider_id).await.ok_or_else(|| {
        HarnessError::invalid(format!("unknown provider profile {provider_id:?}"))
    })?;
    provider
        .resolved_model(model)?
        .reasoning_value(*reasoning_effort)?;
    Ok(Some(provider))
}

#[expect(
    clippy::too_many_lines,
    reason = "session model and plugin overrides are composed together in precedence order"
)]
pub(super) fn session_profile(
    base: &Profile,
    model: &ModelSelection,
    server_model: Option<&ternilo_protocol::RunModelSnapshot>,
    named_provider: Option<&ProviderProfile>,
    preset_plugins: &[PluginEntry],
    profile_plugins: &[PluginEntry],
    mode: SessionMode,
) -> Profile {
    let mut layers = vec![
        base.clone(),
        Profile {
            plugins: preset_plugins.to_vec(),
        },
        Profile {
            plugins: profile_plugins.to_vec(),
        },
    ];
    if mode == SessionMode::Plan {
        layers.push(Profile {
            plugins: vec![PluginEntry {
                id: "session-mode".to_owned(),
                kind: ternilo_builtins::PROMPT_SECTION_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({
                    "id": "plan-mode",
                    "order": 900,
                    "content": "Plan mode is active. Inspect and reason, but do not mutate files, run mutating shell commands, start jobs, terminals, workflows, or subagents. Produce or update a decision-complete plan. The user must switch the session back to execute mode before implementation."
                }),
            }],
        });
    }
    let mut profile = compose_profiles(layers);
    if matches!(
        model,
        ModelSelection::AccountProvider { .. } | ModelSelection::PlatformModel { .. }
    ) {
        if let Some(entry) = profile.plugins.iter_mut().find(|entry| entry.id == "model") {
            ternilo_builtins::BROKERED_MODEL_KIND.clone_into(&mut entry.kind);
            entry.config = serde_json::json!({"snapshot":server_model});
        }
        return profile;
    }
    let route = match (model, named_provider) {
        (
            ModelSelection::OpenAiCompatible {
                base_url,
                model,
                api_key_env,
                timeout_ms,
                max_attempts,
                retry_base_delay_ms,
            },
            _,
        ) => Some((
            "openai-compatible".to_owned(),
            base_url.clone(),
            model.clone(),
            api_key_env.clone(),
            *timeout_ms,
            *max_attempts,
            *retry_base_delay_ms,
            ProviderProtocol::OpenAiChatCompletions,
            None,
            None,
            None,
        )),
        (
            ModelSelection::NamedProvider {
                provider_id,
                model,
                reasoning_effort,
            },
            Some(provider),
        ) => {
            let resolved = provider
                .resolved_model(model)
                .expect("named Provider selection is validated before profile assembly");
            let actual_reasoning = resolved
                .reasoning_value(*reasoning_effort)
                .expect("named Provider reasoning selection is validated before profile assembly")
                .map(str::to_owned);
            Some((
                provider_id.clone(),
                provider.base_url.clone(),
                model.clone(),
                provider.api_key_ref.clone(),
                provider.timeout_ms,
                provider.max_attempts,
                provider.retry_base_delay_ms,
                provider.protocol,
                Some(resolved.context_window),
                Some(
                    u32::try_from(resolved.max_output_tokens)
                        .expect("Provider validation bounds max_output_tokens to u32"),
                ),
                actual_reasoning,
            ))
        }
        _ => None,
    };
    if let Some((
        provider,
        base_url,
        model,
        api_key_env,
        timeout_ms,
        max_attempts,
        retry_base_delay_ms,
        protocol,
        context_window,
        max_tokens,
        reasoning_effort,
    )) = route
        && let Some(entry) = profile.plugins.iter_mut().find(|entry| entry.id == "model")
    {
        "ternilo.model.openai_compatible".clone_into(&mut entry.kind);
        entry.config = serde_json::json!({
            "usage_source": if crate::model_connections::is_connection_provider(&provider) { "connected_server" } else { "direct_provider" },
            "provider": provider,
            "base_url": base_url,
            "model": model,
            "api_key_env": api_key_env,
            "timeout_ms": timeout_ms,
            "max_attempts": max_attempts,
            "retry_base_delay_ms": retry_base_delay_ms,
            "protocol": protocol,
            "context_window": context_window,
            "max_tokens": max_tokens,
            "reasoning_effort": reasoning_effort,
        });
    }
    profile
}
