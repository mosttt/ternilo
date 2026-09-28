#[cfg(test)]
use super::random_hex_128;
use super::{
    ApplicationOperation, Arc, HarnessError, LocalApplication, LocalSessionUpdate, ModelSelection,
    SessionSearchRequest, Value, control_redaction, json,
};

#[cfg(test)]
pub(super) async fn handle_application(
    application: Arc<LocalApplication>,
    request: ApplicationOperation,
) -> Result<Value, HarnessError> {
    let provenance = match &request {
        ApplicationOperation::SessionSubmit { .. }
        | ApplicationOperation::SessionTurn { .. }
        | ApplicationOperation::SessionSkillTurn { .. }
        | ApplicationOperation::SessionSubagentFollowup { .. } => {
            Some(ternilo_protocol::InputProvenance {
                run_id: None,
                input_id: ternilo_protocol::SubmissionId::new(format!(
                    "test-input-{}",
                    random_hex_128()
                )),
                author: ternilo_protocol::InputAuthor::Account {
                    user_id: ternilo_protocol::UserId::new("requesting-user"),
                    username: "requester".to_owned(),
                },
            })
        }
        _ => None,
    };
    handle_application_with_provenance(application, request, provenance).await
}

pub(super) async fn handle_application_with_provenance(
    application: Arc<LocalApplication>,
    request: ApplicationOperation,
    provenance: Option<ternilo_protocol::InputProvenance>,
) -> Result<Value, HarnessError> {
    let roots = control_redaction::operation_roots(&application, &request).await;
    let result = handle_application_inner(application, request, provenance).await;
    control_redaction::application_result(result, &roots)
}

#[allow(clippy::too_many_lines)]
pub(super) async fn handle_application_inner(
    application: Arc<LocalApplication>,
    request: ApplicationOperation,
    provenance: Option<ternilo_protocol::InputProvenance>,
) -> Result<Value, HarnessError> {
    request.validate()?;
    match request {
        ApplicationOperation::Catalog => to_value(application.application_catalog()),
        ApplicationOperation::AgentPresetList => to_value(application.agent_preset_roster().await),
        ApplicationOperation::AgentPresetGet { preset_id } => {
            to_value(application.agent_preset_view(&preset_id).await?)
        }
        ApplicationOperation::AgentPresetCopy { request } => {
            to_value(application.copy_agent_preset(request).await?)
        }
        ApplicationOperation::AgentPresetUpdate { preset_id, request } => {
            to_value(application.update_agent_preset(&preset_id, request).await?)
        }
        ApplicationOperation::AgentPresetDelete { preset_id } => {
            application.remove_agent_preset(&preset_id).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::AgentPresetSetDefault { preset_id } => {
            to_value(application.set_default_agent_preset(&preset_id).await?)
        }
        ApplicationOperation::ExtensionInventory => {
            blocking_application(Arc::clone(&application), |application| {
                to_value(application.extension_inventory()?)
            })
            .await
        }
        ApplicationOperation::PublisherTrust { publisher } => {
            let publisher = serde_json::from_value::<ternilo_extension::PublisherTrust>(publisher)
                .map_err(|error| {
                    HarnessError::invalid(format!("invalid extension publisher trust: {error}"))
                })?;
            blocking_application(Arc::clone(&application), move |application| {
                to_value(application.trust_extension_publisher(publisher)?)
            })
            .await
        }
        ApplicationOperation::PublisherRevoke { key_id } => {
            application.revoke_extension_publisher(&key_id).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::ExtensionInstall { request } => {
            let request =
                serde_json::from_value::<ternilo_extension::ExtensionInstallRequest>(request)
                    .map_err(|error| {
                        HarnessError::invalid(format!("invalid extension install request: {error}"))
                    })?;
            blocking_application(Arc::clone(&application), move |application| {
                to_value(application.install_extension(request)?)
            })
            .await
        }
        ApplicationOperation::ExtensionSetEnabled {
            package_id,
            version,
            enabled,
        } => to_value(
            application
                .set_extension_enabled(&package_id, &version, enabled)
                .await?,
        ),
        ApplicationOperation::ExtensionRevoke {
            package_id,
            version,
        } => {
            application.revoke_extension(&package_id, &version).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::ExtensionUninstall {
            package_id,
            version,
        } => {
            application
                .uninstall_extension(&package_id, &version)
                .await?;
            Ok(Value::Null)
        }
        ApplicationOperation::Snapshot => to_value(application.snapshot().await),
        ApplicationOperation::LiveActivities => to_value(application.live_activities().await),
        ApplicationOperation::CredentialList => to_value(application.credential_inventory().await),
        ApplicationOperation::CredentialSet { name, value } => {
            application.set_credential(name, value).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::CredentialRemove { name } => {
            application.remove_credential(&name).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::CredentialRecordSet { key, kind, payload } => to_value(
            application
                .set_credential_record(key, kind, payload)
                .await?,
        ),
        ApplicationOperation::CredentialRecordDelete { key } => {
            application.delete_credential_record(&key).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::ProviderList => to_value(application.provider_profiles().await),
        ApplicationOperation::ProviderUpsert { provider } => {
            to_value(application.upsert_provider_profile(provider).await?)
        }
        ApplicationOperation::ProviderMaterialize { request } => {
            to_value(application.materialize_extension_provider(request).await?)
        }
        ApplicationOperation::ProviderDelete { id } => {
            application.delete_provider_profile(&id).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::ProviderDiscover { request } => {
            to_value(application.discover_provider_models(request).await?)
        }
        ApplicationOperation::DefaultModelGet => to_value(application.default_model().await?),
        ApplicationOperation::DefaultModelSet { selection } => {
            to_value(application.set_default_model(selection).await?)
        }
        ApplicationOperation::SidebarOrderingGet => to_value(application.sidebar_ordering().await?),
        ApplicationOperation::SidebarOrderingSet { ordering } => {
            to_value(application.set_sidebar_ordering(ordering).await?)
        }
        ApplicationOperation::AuthorizationSnapshot { surface_id } => {
            to_value(application.authorization_snapshot(&surface_id).await?)
        }
        ApplicationOperation::AuthorizationBegin { request } => {
            to_value(application.begin_authorization(request).await?)
        }
        ApplicationOperation::AuthorizationAnswer { answer } => {
            application.answer_authorization_prompt(answer)?;
            Ok(Value::Null)
        }
        ApplicationOperation::AuthorizationCancel { key } => {
            application.cancel_authorization(&key).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::AttachmentResolve { attachment } => {
            to_value(application.resolve_attachment(attachment).await?)
        }
        ApplicationOperation::DirectoryList { path } => {
            to_value(application.list_directory(path.as_deref()).await?)
        }
        ApplicationOperation::DirectoryCreate { parent, name } => {
            Ok(json!({ "path": application.create_directory(&parent, &name).await? }))
        }
        ApplicationOperation::WorkspaceCreate { path } => {
            to_value(application.add_workspace(&path).await?)
        }
        ApplicationOperation::WorkspaceLocation { workspace_id } => {
            let workspace = application
                .snapshot()
                .await
                .workspaces
                .into_iter()
                .find(|workspace| workspace.workspace_id == workspace_id)
                .ok_or_else(|| HarnessError::invalid("local workspace is not registered"))?;
            Ok(json!({
                "path": workspace.path,
                "home": ternilo_local::home_directory().ok().map(|path| path.to_string_lossy().into_owned()),
                "created_at_ms": workspace.created_at_ms,
            }))
        }
        ApplicationOperation::WorkspaceRename {
            workspace_id,
            title,
        } => to_value(application.rename_workspace(workspace_id, title).await?),
        ApplicationOperation::WorkspaceUnregister { workspace_id } => {
            application.unregister_workspace(workspace_id).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::SessionCreate {
            workspace_id,
            session_id,
            agent_id,
            agent_preset,
            permissions,
        } => to_value(
            application
                .create_session_with_options(
                    workspace_id,
                    session_id,
                    agent_id,
                    agent_preset,
                    permissions,
                )
                .await?,
        ),
        ApplicationOperation::SessionUpdate {
            session_id,
            title,
            permissions,
            model,
            server_model,
            agent_preset,
            profile_plugins,
            mode,
        } => {
            let id = session_id.as_str();
            let model = model
                .map(serde_json::from_value::<ModelSelection>)
                .transpose()
                .map_err(|error| {
                    HarnessError::invalid(format!("invalid model selection: {error}"))
                })?;
            to_value(
                application
                    .update_session(
                        id,
                        LocalSessionUpdate {
                            title,
                            permissions,
                            model,
                            server_model,
                            agent_preset,
                            profile_plugins,
                            mode,
                        },
                    )
                    .await?,
            )
        }
        ApplicationOperation::SessionFork { session_id, at_seq } => to_value(
            application
                .fork_session_at(session_id.as_str(), at_seq, None, None)
                .await?,
        ),
        ApplicationOperation::SessionArchive { session_id } => {
            to_value(application.archive_session(session_id.as_str()).await?)
        }
        ApplicationOperation::SessionRestore { session_id } => {
            to_value(application.restore_session(session_id.as_str()).await?)
        }
        ApplicationOperation::SessionDelete { session_id } => {
            application.delete_session(session_id.as_str()).await?;
            Ok(Value::Null)
        }
        ApplicationOperation::SessionEvents {
            session_id,
            after_seq,
        } => to_value(
            application
                .events_after(session_id.as_str(), after_seq)
                .await?,
        ),
        ApplicationOperation::SessionFileContent {
            session_id,
            file_id,
        } => to_value(
            application
                .session_file_content(session_id.as_str(), &file_id)
                .await?,
        ),
        ApplicationOperation::SessionWorkspace {
            session_id,
            request,
        } => to_value(
            application
                .workspace_browser(session_id.as_str(), request)
                .await?,
        ),
        ApplicationOperation::SessionReferenceCandidates {
            session_id,
            request,
        } => to_value(
            application
                .reference_candidates(session_id.as_str(), request)
                .await?,
        ),
        ApplicationOperation::SessionPlugins { session_id } => to_value(
            application
                .effective_session_profile(session_id.as_str())
                .await?,
        ),
        ApplicationOperation::SessionCommands { session_id } => to_value(
            application
                .session_command_catalog(session_id.as_str())
                .await?,
        ),
        ApplicationOperation::SessionServices { session_id } => {
            to_value(application.session_services(session_id.as_str()).await?)
        }
        ApplicationOperation::SessionServiceStart {
            session_id,
            service_id,
        } => to_value(
            application
                .start_session_service(session_id.as_str(), service_id)
                .await?,
        ),
        ApplicationOperation::SessionServiceStop {
            session_id,
            service_id,
        } => to_value(
            application
                .stop_session_service(session_id.as_str(), service_id)
                .await?,
        ),
        ApplicationOperation::SessionProjection { session_id } => {
            to_value(application.session_projection(session_id).await?)
        }
        ApplicationOperation::SessionTelemetry { session_id } => to_value(
            application
                .session_telemetry_sharing(session_id.as_str())
                .await?,
        ),
        ApplicationOperation::SessionSkills { session_id } => {
            to_value(application.skill_catalog(session_id.as_str()).await?)
        }
        ApplicationOperation::SessionSkillResolve {
            session_id,
            name,
            input,
        } => to_value(
            application
                .prepare_skill_invocation(session_id.as_str(), name, input)
                .await?,
        ),
        ApplicationOperation::SessionStats { session_id } => {
            to_value(application.stats(session_id.as_str()).await?)
        }
        ApplicationOperation::SessionExport { session_id } => {
            to_value(application.export_session(session_id.as_str()).await?)
        }
        ApplicationOperation::SessionFeedback {
            session_id,
            target_seq,
            expected_revision,
            rating,
            note,
        } => to_value(
            application
                .record_feedback(
                    session_id.as_str(),
                    target_seq,
                    expected_revision,
                    rating,
                    note,
                )
                .await?,
        ),
        ApplicationOperation::SessionCommandFeedback { session_id, text } => to_value(
            application
                .record_command_feedback(session_id.as_str(), text)
                .await?,
        ),
        ApplicationOperation::SessionSubagentFollowup {
            session_id,
            subagent_id,
            message,
        } => to_value(
            application
                .followup_subagent_with_provenance(
                    session_id.as_str(),
                    subagent_id,
                    message,
                    require_input_provenance(provenance)?,
                )
                .await?,
        ),
        ApplicationOperation::SessionSubagentInterrupt {
            session_id,
            subagent_id,
        } => to_value(
            application
                .interrupt_subagent(session_id.as_str(), subagent_id)
                .await?,
        ),
        ApplicationOperation::SessionAgentTeamSnapshot { session_id } => {
            to_value(application.agent_team_snapshot(session_id.as_str()).await?)
        }
        ApplicationOperation::SessionAgentTeamTaskCreate {
            session_id,
            request,
        } => to_value(
            application
                .create_agent_team_task(session_id.as_str(), request)
                .await?,
        ),
        ApplicationOperation::SessionAgentTeamTaskReplace {
            session_id,
            task_id,
            request,
        } => to_value(
            application
                .replace_agent_team_task(session_id.as_str(), task_id, request)
                .await?,
        ),
        ApplicationOperation::SessionAgentTeamTaskDelete {
            session_id,
            task_id,
            expected_revision,
        } => {
            application
                .delete_agent_team_task(session_id.as_str(), task_id, expected_revision)
                .await?;
            Ok(Value::Null)
        }
        ApplicationOperation::SessionAgentTeamMessageSend {
            session_id,
            request,
        } => to_value(
            application
                .send_agent_team_message(session_id.as_str(), request)
                .await?,
        ),
        ApplicationOperation::SessionAgentTeamMessageRead {
            session_id,
            message_id,
        } => to_value(
            application
                .mark_agent_team_message_read(session_id.as_str(), message_id)
                .await?,
        ),
        ApplicationOperation::SessionInbox { session_id } => {
            to_value(application.session_inbox(session_id.as_str()).await?)
        }
        ApplicationOperation::SessionSubmit {
            session_id,
            request,
        } => to_value(
            application
                .submit_session_with_provenance(
                    session_id.as_str(),
                    request,
                    require_input_provenance(provenance)?,
                )
                .await?,
        ),
        ApplicationOperation::SessionQueueEdit {
            session_id,
            submission_id,
            request,
        } => to_value(
            application
                .edit_session_queue_item(session_id.as_str(), submission_id, request)
                .await?,
        ),
        ApplicationOperation::SessionQueueRemove {
            session_id,
            submission_id,
        } => to_value(
            application
                .remove_session_queue_item(session_id.as_str(), submission_id)
                .await?,
        ),
        ApplicationOperation::SessionQueueSteer {
            session_id,
            submission_id,
        } => to_value(
            application
                .steer_queued_session_item(session_id.as_str(), submission_id)
                .await?,
        ),
        ApplicationOperation::SessionTurn {
            session_id,
            run_id,
            input,
            attachments,
        } => to_value(
            application
                .run_session_input_with_provenance(
                    session_id.as_str(),
                    ternilo_protocol::SessionSubmissionRequest {
                        run_id: run_id.map(ternilo_protocol::RunId::new),
                        content: ternilo_protocol::SubmissionContent::Prompt { input },
                        references: Vec::new(),
                        attachments,
                        delivery: ternilo_protocol::SubmissionDelivery::Queue,
                    },
                    require_input_provenance(provenance)?,
                )
                .await?,
        ),
        ApplicationOperation::SessionSkillTurn {
            session_id,
            run_id,
            name,
            input,
            attachments,
        } => to_value(
            application
                .run_session_input_with_provenance(
                    session_id.as_str(),
                    ternilo_protocol::SessionSubmissionRequest {
                        run_id: run_id.map(ternilo_protocol::RunId::new),
                        content: ternilo_protocol::SubmissionContent::Skill { name, input },
                        references: Vec::new(),
                        attachments,
                        delivery: ternilo_protocol::SubmissionDelivery::Queue,
                    },
                    require_input_provenance(provenance)?,
                )
                .await?,
        ),
        ApplicationOperation::SessionSearch {
            query,
            session_id,
            workspace_id,
            filters,
            limit,
        } => {
            let mut hits = application
                .search_sessions(SessionSearchRequest {
                    query,
                    session_id,
                    workspace_id,
                    filters,
                    limit,
                })
                .await?;
            control_redaction::search_hits(application.as_ref(), &mut hits).await;
            to_value(hits)
        }
        ApplicationOperation::PendingQuestions { session_id } => to_value(
            application
                .pending_questions(session_id.as_ref().map(ternilo_protocol::SessionId::as_str))
                .await,
        ),
        ApplicationOperation::AnswerQuestion { answer } => {
            application.answer_question(answer).await?;
            Ok(Value::Null)
        }
    }
}

async fn blocking_application(
    application: Arc<LocalApplication>,
    operation: impl FnOnce(&LocalApplication) -> Result<Value, HarnessError> + Send + 'static,
) -> Result<Value, HarnessError> {
    tokio::task::spawn_blocking(move || operation(&application))
        .await
        .map_err(|error| {
            HarnessError::execution(format!("join blocking application operation: {error}"))
        })?
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, HarnessError> {
    serde_json::to_value(value).map_err(|error| {
        HarnessError::execution(format!("serialize application response: {error}"))
    })
}

pub(super) fn require_input_provenance(
    provenance: Option<ternilo_protocol::InputProvenance>,
) -> Result<ternilo_protocol::InputProvenance, HarnessError> {
    let provenance = provenance.ok_or_else(|| {
        HarnessError::policy("gateway input command is missing authenticated provenance")
    })?;
    provenance.validate()?;
    if !matches!(
        provenance.author,
        ternilo_protocol::InputAuthor::Account { .. }
    ) {
        return Err(HarnessError::policy(
            "gateway input commands require an authenticated account author",
        ));
    }
    Ok(provenance)
}
