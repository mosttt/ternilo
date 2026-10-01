use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ternilo_control::{
    ControlAction, ControlStore, ControlUser, EdgeSessionRecord, ResourceAccess, ResourceAction,
    ResourceKind,
};
use ternilo_protocol::{
    AgentTeamSnapshot, Attachment, HarnessError, LivePendingQuestion, Profile, QueueEditRequest,
    ReferenceCandidate, ReferenceCandidateRequest, ReferenceCandidateSnapshot, RunId, RunOutcome,
    SessionCommandCatalog, SessionEvent, SessionEventKind, SessionId, SessionInboxSnapshot,
    SessionLiveMetadata, SessionLiveReadMask, SessionProjectionSnapshot, SessionSubmission,
    SessionSubmissionRequest, SubmissionContent, SubmissionId, SubmissionReference, TenantId,
};
use ternilo_transport::ApplicationOperation;

use crate::platform::{http::now_ms, state::AppState};

use super::{
    placement::{PlacementResolver, WorkspaceTarget},
    types::{
        CreateSessionRequest, NodeSessionSnapshot, TurnRequest, UpdateSessionRequest,
        WorkbenchSession,
    },
};

pub(crate) struct EdgeAdapter<'a> {
    state: &'a AppState,
    actor: &'a ControlUser,
    tenant_id: &'a TenantId,
}

pub(crate) async fn authorize_edge_mutation(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
) -> Result<(), HarnessError> {
    state
        .store
        .authorize(actor, tenant_id, ControlAction::RunReserve)
        .await
        .map(drop)
}

impl<'a> EdgeAdapter<'a> {
    pub(crate) fn new(
        state: &'a AppState,
        actor: &'a ControlUser,
        tenant_id: &'a TenantId,
    ) -> Self {
        Self {
            state,
            actor,
            tenant_id,
        }
    }

    pub(crate) async fn create(
        &self,
        request: CreateSessionRequest,
    ) -> Result<WorkbenchSession, HarnessError> {
        let target = PlacementResolver::new(self.state, self.actor, self.tenant_id)
            .workspace(&request.workspace_id)
            .await?;
        let WorkspaceTarget::Edge {
            workspace,
            executor_id,
            node_workspace_id,
        } = target
        else {
            return Err(HarnessError::policy(
                "cloud Workspace cannot be routed through a local Node",
            ));
        };
        self.state
            .store
            .resource_access(
                self.actor,
                self.tenant_id,
                ResourceKind::Workspace,
                workspace.workspace_id.as_str(),
            )
            .await?
            .require(ResourceAction::Submit)?;
        let requested_session_id = request.session_id.as_deref().map(SessionId::new);
        let _resources = self
            .state
            .edge
            .lock_resources(self.tenant_id, &executor_id)
            .await;
        if let Some(session_id) = &requested_session_id {
            PlacementResolver::new(self.state, self.actor, self.tenant_id)
                .ensure_session_id_available(session_id)
                .await?;
        }
        let value = self
            .state
            .edge
            .call(
                self.tenant_id,
                &executor_id,
                ApplicationOperation::SessionCreate {
                    workspace_id: node_workspace_id.clone(),
                    session_id: None,
                    agent_id: request.agent_id,
                    agent_preset: request.agent_preset,
                    permissions: request.permissions,
                },
            )
            .await?;
        let snapshot: NodeSessionSnapshot = decode(value, "Node Session create response")?;
        validate_node_snapshot(&snapshot, &node_workspace_id, None)?;
        let node_session_id = snapshot.identity.session_id.clone();
        let mapping = match self
            .state
            .store
            .create_edge_session_mapping(
                self.actor,
                self.tenant_id,
                &workspace.workspace_id,
                &executor_id,
                &node_session_id,
                requested_session_id.as_ref(),
                snapshot.metadata(None),
                now_ms()?,
            )
            .await
        {
            Ok(mapping) => mapping,
            Err(error) => {
                let _ = self
                    .state
                    .edge
                    .call(
                        self.tenant_id,
                        &executor_id,
                        ApplicationOperation::SessionDelete {
                            session_id: node_session_id,
                        },
                    )
                    .await;
                return Err(error);
            }
        };
        self.present(mapping, format!("此电脑 / {}", workspace.name))
            .await
    }

    pub(crate) async fn update(
        &self,
        mapping: EdgeSessionRecord,
        update: UpdateSessionRequest,
    ) -> Result<WorkbenchSession, HarnessError> {
        if update.model_token_limit.is_some() {
            return Err(HarnessError::invalid(
                "task token limits apply to managed sessions",
            ));
        }
        if !update.has_changes() {
            return Err(HarnessError::invalid("session update has no fields"));
        }
        self.require_action(&mapping, ResourceAction::Configure)
            .await?;
        let _resources = self
            .state
            .edge
            .lock_resources(self.tenant_id, &mapping.executor_id)
            .await;
        let model_changed = update.model.is_some();
        let server_model = match &update.model {
            Some(model) => {
                super::model_options::resolve_edge_selection(
                    self.state,
                    self.actor,
                    self.tenant_id,
                    &mapping,
                    model,
                )
                .await?
            }
            None => None,
        };
        if model_changed {
            self.state
                .store
                .set_edge_session_model_snapshot(
                    self.actor,
                    self.tenant_id,
                    &mapping.session_id,
                    server_model.clone(),
                )
                .await?;
        }
        let value = self
            .state
            .edge
            .call(
                self.tenant_id,
                &mapping.executor_id,
                ApplicationOperation::SessionUpdate {
                    session_id: mapping.node_session_id.clone(),
                    title: update.title,
                    permissions: update.permissions,
                    model: update.model,
                    server_model,
                    agent_preset: update.agent_preset,
                    profile_plugins: update.profile_plugins,
                    mode: update.mode,
                },
            )
            .await;
        let value = match value {
            Ok(value) => value,
            Err(error) => {
                if model_changed {
                    self.state
                        .store
                        .set_edge_session_model_snapshot(
                            self.actor,
                            self.tenant_id,
                            &mapping.session_id,
                            mapping.metadata.server_model.clone(),
                        )
                        .await?;
                }
                return Err(error);
            }
        };
        self.record_action(&mapping, ResourceAction::Configure)
            .await?;
        self.cache_returned_snapshot(mapping, value, None).await
    }

    pub(crate) async fn fork(
        &self,
        mapping: EdgeSessionRecord,
        at_seq: Option<u64>,
    ) -> Result<WorkbenchSession, HarnessError> {
        self.require_action(&mapping, ResourceAction::Submit)
            .await?;
        let _resources = self
            .state
            .edge
            .lock_resources(self.tenant_id, &mapping.executor_id)
            .await;
        let value = self
            .state
            .edge
            .call(
                self.tenant_id,
                &mapping.executor_id,
                ApplicationOperation::SessionFork {
                    session_id: mapping.node_session_id.clone(),
                    at_seq,
                },
            )
            .await?;
        let snapshot: NodeSessionSnapshot = decode(value, "Node Session fork response")?;
        let node_workspace = self.node_workspace_id(&mapping).await?;
        validate_node_snapshot(&snapshot, &node_workspace, None)?;
        if snapshot.parent_session_id.as_ref() != Some(&mapping.node_session_id) {
            return Err(HarnessError::policy(
                "Node fork response changed the resolved parent Session",
            ));
        }
        let child_node_session = snapshot.identity.session_id.clone();
        let child = match self
            .state
            .store
            .create_edge_session_mapping(
                self.actor,
                self.tenant_id,
                &mapping.workspace_id,
                &mapping.executor_id,
                &child_node_session,
                None,
                snapshot.metadata(Some(mapping.session_id)),
                now_ms()?,
            )
            .await
        {
            Ok(child) => child,
            Err(error) => {
                let _ = self
                    .state
                    .edge
                    .call(
                        self.tenant_id,
                        &mapping.executor_id,
                        ApplicationOperation::SessionDelete {
                            session_id: child_node_session,
                        },
                    )
                    .await;
                return Err(error);
            }
        };
        self.present(child, edge_workspace_path()).await
    }

    pub(crate) async fn archive(
        &self,
        mapping: EdgeSessionRecord,
    ) -> Result<WorkbenchSession, HarnessError> {
        self.require_action(&mapping, ResourceAction::Delete)
            .await?;
        let value = self
            .state
            .edge
            .call(
                self.tenant_id,
                &mapping.executor_id,
                ApplicationOperation::SessionArchive {
                    session_id: mapping.node_session_id.clone(),
                },
            )
            .await?;
        let parent = mapping.metadata.parent_session_id.clone();
        self.record_action(&mapping, ResourceAction::Delete).await?;
        self.cache_returned_snapshot(mapping, value, parent).await
    }

    pub(crate) async fn restore(
        &self,
        mapping: EdgeSessionRecord,
    ) -> Result<WorkbenchSession, HarnessError> {
        self.require_action(&mapping, ResourceAction::Delete)
            .await?;
        let _resources = self
            .state
            .edge
            .lock_resources(self.tenant_id, &mapping.executor_id)
            .await;
        let value = self
            .state
            .edge
            .call(
                self.tenant_id,
                &mapping.executor_id,
                ApplicationOperation::SessionRestore {
                    session_id: mapping.node_session_id.clone(),
                },
            )
            .await?;
        let parent = mapping.metadata.parent_session_id.clone();
        self.record_action(&mapping, ResourceAction::Delete).await?;
        self.cache_returned_snapshot(mapping, value, parent).await
    }

    pub(crate) async fn delete(&self, mapping: EdgeSessionRecord) -> Result<(), HarnessError> {
        self.require_action(&mapping, ResourceAction::Delete)
            .await?;
        let _resources = self
            .state
            .edge
            .lock_resources(self.tenant_id, &mapping.executor_id)
            .await;
        self.state
            .edge
            .call(
                self.tenant_id,
                &mapping.executor_id,
                ApplicationOperation::SessionDelete {
                    session_id: mapping.node_session_id.clone(),
                },
            )
            .await?;
        self.state
            .store
            .delete_edge_session_mapping(
                self.actor,
                self.tenant_id,
                &mapping.session_id,
                &mapping.node_session_id,
                now_ms()?,
            )
            .await
    }

    pub(crate) async fn history(
        &self,
        mapping: &EdgeSessionRecord,
        query: ternilo_protocol::SessionHistoryQuery,
    ) -> Result<ternilo_protocol::SessionEventPage, HarnessError> {
        query.validate()?;
        self.require_action(mapping, ResourceAction::View).await?;
        let mut page = if self
            .state
            .edge
            .is_connected(self.tenant_id, &mapping.executor_id)
            .await
        {
            let value = self
                .state
                .edge
                .call(
                    self.tenant_id,
                    &mapping.executor_id,
                    ApplicationOperation::SessionHistory {
                        session_id: mapping.node_session_id.clone(),
                        query,
                    },
                )
                .await?;
            let mut page: ternilo_protocol::SessionEventPage =
                decode(value, "Node Session history response")?;
            self.state
                .store
                .edge_store()
                .project_event_provenance(
                    self.tenant_id,
                    &mapping.executor_id,
                    &mapping.node_session_id,
                    &mut page.events,
                )
                .await?;
            page
        } else {
            self.state
                .store
                .edge_store()
                .history(
                    self.tenant_id,
                    &mapping.executor_id,
                    &mapping.node_session_id,
                    query,
                )
                .await?
        };
        self.translate_event_references_from_node(mapping, &mut page.events)
            .await?;
        Ok(page)
    }

    pub(crate) async fn refresh_event_delta(
        &self,
        mapping: &EdgeSessionRecord,
        after_seq: Option<u64>,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        self.require_action(mapping, ResourceAction::View).await?;
        if !self
            .state
            .edge
            .is_connected(self.tenant_id, &mapping.executor_id)
            .await
        {
            return self.live_event_delta(mapping, after_seq).await;
        }
        let value = self
            .state
            .edge
            .call(
                self.tenant_id,
                &mapping.executor_id,
                ApplicationOperation::SessionEvents {
                    session_id: mapping.node_session_id.clone(),
                    after_seq,
                },
            )
            .await?;
        let mut events: Vec<SessionEvent> = decode(value, "Node Session events response")?;
        self.state
            .store
            .edge_store()
            .project_event_provenance(
                self.tenant_id,
                &mapping.executor_id,
                &mapping.node_session_id,
                &mut events,
            )
            .await?;
        self.translate_event_references_from_node(mapping, &mut events)
            .await?;
        Ok(events)
    }

    pub(crate) async fn events(
        &self,
        mapping: &EdgeSessionRecord,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        self.require_action(mapping, ResourceAction::View).await?;
        if self
            .state
            .edge
            .is_connected(self.tenant_id, &mapping.executor_id)
            .await
        {
            let value = self
                .state
                .edge
                .call(
                    self.tenant_id,
                    &mapping.executor_id,
                    ApplicationOperation::SessionEvents {
                        session_id: mapping.node_session_id.clone(),
                        after_seq: self
                            .state
                            .store
                            .edge_store()
                            .last_event_seq(
                                self.tenant_id,
                                &mapping.executor_id,
                                &mapping.node_session_id,
                            )
                            .await?
                            .and_then(|seq| seq.checked_sub(1)),
                    },
                )
                .await?;
            let events: Vec<SessionEvent> = decode(value, "Node Session events response")?;
            self.state
                .store
                .edge_store()
                .merge_events(
                    self.tenant_id,
                    &mapping.executor_id,
                    &mapping.node_session_id,
                    &events,
                )
                .await?;
        }
        let mut events = self
            .state
            .edge
            .cached_events(
                self.tenant_id,
                &mapping.executor_id,
                &mapping.node_session_id,
            )
            .await?;
        self.translate_event_references_from_node(mapping, &mut events)
            .await?;
        Ok(events)
    }

    pub(crate) async fn live_event_delta(
        &self,
        mapping: &EdgeSessionRecord,
        after_seq: Option<u64>,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        self.require_action(mapping, ResourceAction::View).await?;
        let mut events = self
            .state
            .edge
            .cached_event_delta(
                self.tenant_id,
                &mapping.executor_id,
                &mapping.node_session_id,
                after_seq,
                Duration::ZERO,
            )
            .await?;
        self.translate_event_references_from_node(mapping, &mut events)
            .await?;
        Ok(events)
    }

    pub(crate) async fn call_session(
        &self,
        mapping: &EdgeSessionRecord,
        operation: impl FnOnce(SessionId) -> ApplicationOperation,
    ) -> Result<Value, HarnessError> {
        let operation = operation(mapping.node_session_id.clone());
        let action = if let ApplicationOperation::AnswerQuestion { answer } = &operation {
            self.require_action(mapping, ResourceAction::View).await?;
            let value = self
                .state
                .edge
                .call(
                    self.tenant_id,
                    &mapping.executor_id,
                    ApplicationOperation::PendingQuestions {
                        session_id: Some(mapping.node_session_id.clone()),
                    },
                )
                .await?;
            let questions: Vec<LivePendingQuestion> =
                decode(value, "Node pending questions response")?;
            let pending = questions
                .iter()
                .find(|pending| {
                    pending.session_id == mapping.node_session_id
                        && pending.question.id == answer.question_id
                })
                .ok_or_else(|| {
                    HarnessError::invalid("pending question does not belong to this session")
                })?;
            ternilo_control::question_resource_action(&pending.question)
        } else {
            operation_action(&operation)
        };
        self.require_action(mapping, action).await?;
        let value = self
            .state
            .edge
            .call_as(self.tenant_id, &mapping.executor_id, self.actor, operation)
            .await?;
        if action != ResourceAction::View {
            self.record_action(mapping, action).await?;
        }
        Ok(value)
    }

    pub(crate) async fn call_session_mutation(
        &self,
        mapping: &EdgeSessionRecord,
        operation: impl FnOnce(SessionId) -> ApplicationOperation,
    ) -> Result<Value, HarnessError> {
        self.call_session(mapping, operation).await
    }

    pub(crate) async fn command_catalog(
        &self,
        mapping: &EdgeSessionRecord,
    ) -> Result<SessionCommandCatalog, HarnessError> {
        let value = self
            .call_session(mapping, |session_id| {
                ApplicationOperation::SessionCommands { session_id }
            })
            .await?;
        let catalog: SessionCommandCatalog =
            decode(value, "Node Session command catalog response")?;
        expose_command_catalog(catalog, &mapping.node_session_id, &mapping.session_id)
    }

    pub(crate) async fn resolve_attachment(
        &self,
        mapping: &EdgeSessionRecord,
        attachment: Attachment,
    ) -> Result<Attachment, HarnessError> {
        self.require_action(mapping, ResourceAction::View).await?;
        attachment.validate()?;
        self.require_attachment_references(mapping, std::slice::from_ref(&attachment))
            .await?;
        let value = self
            .state
            .edge
            .call(
                self.tenant_id,
                &mapping.executor_id,
                ApplicationOperation::AttachmentResolve { attachment },
            )
            .await?;
        let resolved: Attachment = decode(value, "Node attachment resolve response")?;
        resolved.validate()?;
        Ok(resolved)
    }

    async fn require_attachment_references(
        &self,
        mapping: &EdgeSessionRecord,
        attachments: &[Attachment],
    ) -> Result<(), HarnessError> {
        let digests = attachments
            .iter()
            .filter_map(Attachment::reference_digest)
            .collect::<Vec<_>>();
        if digests.is_empty()
            || self
                .require_action(mapping, ResourceAction::View)
                .await?
                .is_execution_owner
        {
            return Ok(());
        }
        let (events, inbox) = tokio::try_join!(self.events(mapping), self.inbox(mapping))?;
        for digest in digests {
            let retained = events
                .iter()
                .any(|event| ternilo_control::event_references_attachment(event, digest))
                || inbox.items.iter().any(|item| {
                    item.attachments
                        .iter()
                        .any(|attachment| attachment.reference_digest() == Some(digest))
                });
            if !retained {
                return Err(HarnessError::policy(
                    "attachment reference does not belong to the shared session",
                ));
            }
        }
        Ok(())
    }

    pub(crate) async fn inbox(
        &self,
        mapping: &EdgeSessionRecord,
    ) -> Result<SessionInboxSnapshot, HarnessError> {
        let value = self
            .call_session(mapping, |session_id| ApplicationOperation::SessionInbox {
                session_id,
            })
            .await?;
        let mut snapshot: SessionInboxSnapshot = decode(value, "Node Session inbox response")?;
        validate_node_inbox(&snapshot, &mapping.node_session_id)?;
        snapshot.session_id = mapping.session_id.clone();
        for item in &mut snapshot.items {
            self.translate_submission_references_from_node(mapping, item)
                .await?;
        }
        Ok(snapshot)
    }

    pub(crate) async fn live_metadata(
        &self,
        mapping: &EdgeSessionRecord,
        read: SessionLiveReadMask,
    ) -> Result<SessionLiveMetadata, HarnessError> {
        let mut metadata = SessionLiveMetadata {
            read,
            ..SessionLiveMetadata::default()
        };
        if read.inbox {
            metadata.inbox = Some(self.inbox(mapping).await?);
        }
        if read.stats {
            metadata.stats = Some(decode(
                self.call_session(mapping, |session_id| ApplicationOperation::SessionStats {
                    session_id,
                })
                .await?,
                "Node Session stats response",
            )?);
        }
        if read.projection {
            let mut projection: SessionProjectionSnapshot = decode(
                self.call_session(mapping, |session_id| {
                    ApplicationOperation::SessionProjection { session_id }
                })
                .await?,
                "Node Session projection response",
            )?;
            if projection.session_id != mapping.node_session_id {
                return Err(HarnessError::policy(
                    "Node Session projection changed the resolved Session",
                ));
            }
            projection.session_id.clone_from(&mapping.session_id);
            metadata.projection = Some(projection);
        }
        if read.questions {
            metadata.questions = Some(self.pending_questions(mapping).await?);
        }
        if read.profile {
            metadata.profile = Some(decode::<Profile>(
                self.call_session(mapping, |session_id| ApplicationOperation::SessionPlugins {
                    session_id,
                })
                .await?,
                "Node Session profile response",
            )?);
        }
        if read.agent_team {
            metadata.agent_team = Some(decode::<AgentTeamSnapshot>(
                self.call_session(mapping, |session_id| {
                    ApplicationOperation::SessionAgentTeamSnapshot { session_id }
                })
                .await?,
                "Node Agent Team response",
            )?);
        }
        Ok(metadata)
    }

    pub(crate) async fn pending_questions(
        &self,
        mapping: &EdgeSessionRecord,
    ) -> Result<Vec<LivePendingQuestion>, HarnessError> {
        let mut questions: Vec<LivePendingQuestion> = decode(
            self.call_session(mapping, |session_id| {
                ApplicationOperation::PendingQuestions {
                    session_id: Some(session_id),
                }
            })
            .await?,
            "Node pending questions response",
        )?;
        for question in &mut questions {
            if question.session_id != mapping.node_session_id {
                return Err(HarnessError::policy(
                    "Node pending questions changed the resolved Session",
                ));
            }
            question.session_id.clone_from(&mapping.session_id);
        }
        Ok(questions)
    }

    pub(crate) async fn reference_candidates(
        &self,
        mapping: &EdgeSessionRecord,
        request: ReferenceCandidateRequest,
    ) -> Result<Value, HarnessError> {
        let value = self
            .call_session(mapping, |session_id| {
                ApplicationOperation::SessionReferenceCandidates {
                    session_id,
                    request,
                }
            })
            .await?;
        let mut snapshot: ReferenceCandidateSnapshot =
            decode(value, "Node reference candidate response")?;
        let mappings = self
            .state
            .store
            .list_accessible_edge_sessions(self.actor, self.tenant_id)
            .await?;
        snapshot.candidates.retain_mut(|candidate| {
            let ReferenceCandidate::Session { session_id, .. } = candidate else {
                return true;
            };
            let Some(source) = mappings.iter().find(|source| {
                source.executor_id == mapping.executor_id && source.node_session_id == *session_id
            }) else {
                return false;
            };
            session_id.clone_from(&source.session_id);
            true
        });
        serde_json::to_value(snapshot).map_err(|error| {
            HarnessError::execution(format!("encode Node reference candidates: {error}"))
        })
    }

    pub(crate) async fn submit_inbox(
        &self,
        mapping: &EdgeSessionRecord,
        mut request: SessionSubmissionRequest,
    ) -> Result<SessionSubmission, HarnessError> {
        self.require_action(mapping, submission_action(&request.content))
            .await?;
        if request.delivery == ternilo_protocol::SubmissionDelivery::Steer {
            self.require_action(mapping, ResourceAction::Stop).await?;
        }
        self.require_attachment_references(mapping, &request.attachments)
            .await?;
        self.mark_session_started(
            mapping,
            &submission_display(&request.content),
            &request.attachments,
        )
        .await?;
        let expected_run = request.run_id.clone();
        self.translate_references_to_node(mapping, &mut request.references)
            .await?;
        let value = self
            .call_session(mapping, |session_id| ApplicationOperation::SessionSubmit {
                session_id,
                request,
            })
            .await?;
        let mut submission: SessionSubmission = decode(value, "Node Session submission response")?;
        submission.validate()?;
        if expected_run.is_some_and(|run_id| run_id != submission.run_id) {
            return Err(HarnessError::policy(
                "Node changed the requested Queue run identifier",
            ));
        }
        self.translate_submission_references_from_node(mapping, &mut submission)
            .await?;
        Ok(submission)
    }

    async fn translate_references_to_node(
        &self,
        mapping: &EdgeSessionRecord,
        references: &mut [SubmissionReference],
    ) -> Result<(), HarnessError> {
        let mappings = self
            .state
            .store
            .list_accessible_edge_sessions(self.actor, self.tenant_id)
            .await?;
        for reference in references {
            let SubmissionReference::Session { session_id, .. } = reference else {
                continue;
            };
            let source = mappings
                .iter()
                .find(|source| source.session_id == *session_id)
                .ok_or_else(|| HarnessError::invalid("referenced Session is unavailable"))?;
            if source.executor_id != mapping.executor_id {
                return Err(HarnessError::invalid(
                    "a local Node Session can only reference Sessions on the same Node",
                ));
            }
            session_id.clone_from(&source.node_session_id);
        }
        Ok(())
    }

    async fn translate_submission_references_from_node(
        &self,
        mapping: &EdgeSessionRecord,
        submission: &mut SessionSubmission,
    ) -> Result<(), HarnessError> {
        self.state
            .store
            .edge_store()
            .project_submission_provenance(
                self.tenant_id,
                &mapping.executor_id,
                &mapping.node_session_id,
                submission,
            )
            .await?;
        let mappings = self
            .state
            .store
            .list_accessible_edge_sessions(self.actor, self.tenant_id)
            .await?;
        translate_reference_ids_from_node(mapping, &mappings, &mut submission.references);
        Ok(())
    }

    async fn translate_event_references_from_node(
        &self,
        mapping: &EdgeSessionRecord,
        events: &mut [SessionEvent],
    ) -> Result<(), HarnessError> {
        let mappings = self
            .state
            .store
            .list_accessible_edge_sessions(self.actor, self.tenant_id)
            .await?;
        for event in events {
            if let SessionEventKind::UserMessage { references, .. } = &mut event.kind {
                translate_reference_ids_from_node(mapping, &mappings, references);
            }
            if let SessionEventKind::ProviderUsageStarted {
                source_session_id, ..
            } = &mut event.kind
            {
                *source_session_id = source_session_id.as_ref().and_then(|id| {
                    if id == &mapping.node_session_id {
                        Some(mapping.session_id.clone())
                    } else {
                        mappings
                            .iter()
                            .find(|source| {
                                source.executor_id == mapping.executor_id
                                    && source.node_session_id == *id
                            })
                            .map(|source| source.session_id.clone())
                    }
                });
            }
        }
        // A Node read can outlive a share; recheck before releasing its projected events.
        self.require_action(mapping, ResourceAction::View).await?;
        Ok(())
    }

    pub(crate) async fn edit_inbox(
        &self,
        mapping: &EdgeSessionRecord,
        submission_id: SubmissionId,
        request: QueueEditRequest,
    ) -> Result<SessionSubmission, HarnessError> {
        let inbox = self.inbox(mapping).await?;
        let item = inbox
            .items
            .iter()
            .find(|item| item.id == submission_id)
            .ok_or_else(|| HarnessError::invalid("queued submission does not exist"))?;
        let action = match item.content.skill_name() {
            None => prompt_action(&request.input),
            Some(_) => ResourceAction::Submit,
        };
        self.require_action(mapping, action).await?;
        let expected = submission_id.clone();
        let value = self
            .call_session_mutation(mapping, |session_id| {
                ApplicationOperation::SessionQueueEdit {
                    session_id,
                    submission_id,
                    request,
                }
            })
            .await?;
        let mut submission = decode_exact_submission(value, &expected, "Node Queue edit response")?;
        self.translate_submission_references_from_node(mapping, &mut submission)
            .await?;
        Ok(submission)
    }

    pub(crate) async fn remove_inbox(
        &self,
        mapping: &EdgeSessionRecord,
        submission_id: SubmissionId,
    ) -> Result<SessionSubmission, HarnessError> {
        let expected = submission_id.clone();
        let value = self
            .call_session_mutation(mapping, |session_id| {
                ApplicationOperation::SessionQueueRemove {
                    session_id,
                    submission_id,
                }
            })
            .await?;
        let mut submission =
            decode_exact_submission(value, &expected, "Node Queue remove response")?;
        self.translate_submission_references_from_node(mapping, &mut submission)
            .await?;
        Ok(submission)
    }

    pub(crate) async fn steer_inbox(
        &self,
        mapping: &EdgeSessionRecord,
        submission_id: SubmissionId,
    ) -> Result<SessionSubmission, HarnessError> {
        self.require_action(mapping, ResourceAction::Submit).await?;
        let expected = submission_id.clone();
        let value = self
            .call_session_mutation(mapping, |session_id| {
                ApplicationOperation::SessionQueueSteer {
                    session_id,
                    submission_id,
                }
            })
            .await?;
        let mut submission =
            decode_exact_submission(value, &expected, "Node Queue steering response")?;
        self.translate_submission_references_from_node(mapping, &mut submission)
            .await?;
        Ok(submission)
    }

    pub(crate) async fn turn(
        &self,
        mapping: &EdgeSessionRecord,
        request: TurnRequest,
        skill: Option<String>,
    ) -> Result<Value, HarnessError> {
        self.require_action(
            mapping,
            if skill.is_some() {
                ResourceAction::Submit
            } else {
                prompt_action(&request.input)
            },
        )
        .await?;
        self.require_attachment_references(mapping, &request.attachments)
            .await?;
        self.mark_session_started(mapping, &request.input, &request.attachments)
            .await?;
        let operation = if let Some(name) = skill {
            ApplicationOperation::SessionSkillTurn {
                session_id: mapping.node_session_id.clone(),
                run_id: request.run_id,
                name,
                input: request.input,
                attachments: request.attachments,
            }
        } else {
            ApplicationOperation::SessionTurn {
                session_id: mapping.node_session_id.clone(),
                run_id: request.run_id,
                input: request.input,
                attachments: request.attachments,
            }
        };
        let value = self.call_session(mapping, |_| operation).await?;
        let mut outcome: RunOutcome = decode(value, "Node turn response")?;
        self.state
            .store
            .edge_store()
            .project_event_provenance(
                self.tenant_id,
                &mapping.executor_id,
                &mapping.node_session_id,
                &mut outcome.events,
            )
            .await?;
        self.translate_event_references_from_node(mapping, &mut outcome.events)
            .await?;
        if let Some(title) = &outcome.generated_title {
            self.state
                .store
                .apply_generated_edge_title(
                    self.actor,
                    self.tenant_id,
                    &mapping.session_id,
                    &mapping.node_session_id,
                    title,
                    now_ms()?,
                )
                .await?;
        }
        serde_json::to_value(outcome).map_err(|error| {
            HarnessError::execution(format!("serialize Node turn response: {error}"))
        })
    }

    async fn mark_session_started(
        &self,
        mapping: &EdgeSessionRecord,
        _display: &str,
        _attachments: &[Attachment],
    ) -> Result<(), HarnessError> {
        if !mapping.metadata.blank {
            return Ok(());
        }
        let mut metadata = mapping.metadata.clone();
        metadata.blank = false;
        let updated_at_ms = now_ms()?;
        metadata.updated_at_ms = updated_at_ms;
        self.state
            .store
            .update_edge_session_metadata(
                self.actor,
                self.tenant_id,
                &mapping.session_id,
                &mapping.node_session_id,
                metadata,
                updated_at_ms,
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn cancel(
        &self,
        mapping: &EdgeSessionRecord,
        run_id: RunId,
    ) -> Result<(), HarnessError> {
        self.require_action(mapping, ResourceAction::Stop).await?;
        self.state
            .edge
            .cancel_run(
                self.tenant_id,
                &mapping.executor_id,
                mapping.node_session_id.clone(),
                run_id,
            )
            .await?;
        self.record_action(mapping, ResourceAction::Stop).await
    }

    async fn require_action(
        &self,
        mapping: &EdgeSessionRecord,
        action: ResourceAction,
    ) -> Result<ResourceAccess, HarnessError> {
        let access = self
            .state
            .store
            .resource_access(
                self.actor,
                self.tenant_id,
                ResourceKind::Session,
                mapping.session_id.as_str(),
            )
            .await?;
        access.require(ResourceAction::View)?;
        access.require(action)?;
        Ok(access)
    }

    async fn record_action(
        &self,
        mapping: &EdgeSessionRecord,
        action: ResourceAction,
    ) -> Result<(), HarnessError> {
        let mut transaction = self
            .state
            .store
            .database()
            .tenant_transaction(self.tenant_id)
            .await?;
        ControlStore::record_resource_action_in(
            &mut transaction,
            &self.actor.user_id,
            self.tenant_id,
            ResourceKind::Session,
            mapping.session_id.as_str(),
            &mapping.owner_user_id,
            action,
            now_ms()?,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(ternilo_storage::database_error)
    }

    async fn present(
        &self,
        mapping: EdgeSessionRecord,
        path: String,
    ) -> Result<WorkbenchSession, HarnessError> {
        let access = self.require_action(&mapping, ResourceAction::View).await?;
        Ok(WorkbenchSession::edge(mapping, path).with_access(access))
    }

    pub(crate) async fn sanitized_export(
        &self,
        mapping: &EdgeSessionRecord,
    ) -> Result<Value, HarnessError> {
        let events = self.events(mapping).await?;
        Ok(json!({
            "schema_version": 1,
            "session": self.present(mapping.clone(), edge_workspace_path()).await?,
            "workspace": {
                "workspace_id": mapping.workspace_id,
                "path": edge_workspace_path(),
                "title": "此电脑 Workspace",
            },
            "events": events,
        }))
    }

    async fn cache_returned_snapshot(
        &self,
        mapping: EdgeSessionRecord,
        value: Value,
        parent: Option<SessionId>,
    ) -> Result<WorkbenchSession, HarnessError> {
        let snapshot: NodeSessionSnapshot = decode(value, "Node Session response")?;
        let node_workspace = self.node_workspace_id(&mapping).await?;
        validate_node_snapshot(&snapshot, &node_workspace, Some(&mapping.node_session_id))?;
        let updated = self
            .state
            .store
            .update_edge_session_metadata(
                self.actor,
                self.tenant_id,
                &mapping.session_id,
                &mapping.node_session_id,
                snapshot.metadata(parent),
                now_ms()?,
            )
            .await?;
        self.present(updated, edge_workspace_path()).await
    }

    async fn node_workspace_id(
        &self,
        mapping: &EdgeSessionRecord,
    ) -> Result<ternilo_protocol::WorkspaceId, HarnessError> {
        // Registered and unregistered Sessions both keep their immutable Node
        // binding. The workspace row is retained by logical unregister.
        let workspace = self
            .state
            .store
            .resolve_accessible_session_workspace(self.actor, self.tenant_id, &mapping.session_id)
            .await?;
        if workspace.executor_id.as_ref() != Some(&mapping.executor_id) {
            return Err(HarnessError::execution(
                "edge Session and Workspace executor bindings conflict",
            ));
        }
        workspace
            .executor_workspace_id
            .ok_or_else(|| HarnessError::execution("local-node Workspace binding is incomplete"))
    }
}

fn submission_action(content: &SubmissionContent) -> ResourceAction {
    match content.skill_name() {
        None => prompt_action(content.input()),
        Some(_) => ResourceAction::Submit,
    }
}

fn prompt_action(input: &str) -> ResourceAction {
    let command = input
        .trim_start()
        .strip_prefix('/')
        .and_then(|text| text.split_whitespace().next());
    match command {
        Some("extension-enable" | "extension-disable" | "extension-revoke") => {
            ResourceAction::ManageExecution
        }
        Some("extension-mount" | "extension-unmount" | "plan" | "exit-plan") => {
            ResourceAction::Configure
        }
        Some("agent-stop" | "job-kill" | "terminal-close" | "schedule-delete") => {
            ResourceAction::Stop
        }
        _ => ResourceAction::Submit,
    }
}

fn operation_action(operation: &ApplicationOperation) -> ResourceAction {
    match operation {
        ApplicationOperation::Catalog
        | ApplicationOperation::DefaultModelGet
        | ApplicationOperation::SessionEvents { .. }
        | ApplicationOperation::SessionFileContent { .. }
        | ApplicationOperation::SessionReferenceCandidates { .. }
        | ApplicationOperation::SessionWorkspace { .. }
        | ApplicationOperation::SessionPlugins { .. }
        | ApplicationOperation::SessionCommands { .. }
        | ApplicationOperation::SessionServices { .. }
        | ApplicationOperation::SessionProjection { .. }
        | ApplicationOperation::SessionTelemetry { .. }
        | ApplicationOperation::SessionSkills { .. }
        | ApplicationOperation::SessionSkillResolve { .. }
        | ApplicationOperation::SessionStats { .. }
        | ApplicationOperation::SessionExport { .. }
        | ApplicationOperation::SessionAgentTeamSnapshot { .. }
        | ApplicationOperation::SessionInbox { .. }
        | ApplicationOperation::PendingQuestions { .. } => ResourceAction::View,
        ApplicationOperation::SessionUpdate { .. } => ResourceAction::Configure,
        ApplicationOperation::SessionArchive { .. }
        | ApplicationOperation::SessionRestore { .. }
        | ApplicationOperation::SessionDelete { .. } => ResourceAction::Delete,
        ApplicationOperation::SessionSubagentInterrupt { .. }
        | ApplicationOperation::SessionServiceStop { .. }
        | ApplicationOperation::SessionQueueSteer { .. }
        | ApplicationOperation::SessionQueueRemove { .. } => ResourceAction::Stop,
        ApplicationOperation::SessionSubmit { request, .. } => submission_action(&request.content),
        ApplicationOperation::SessionTurn { input, .. } => prompt_action(input),
        ApplicationOperation::SessionSubagentFollowup { message, .. } => prompt_action(message),
        ApplicationOperation::SessionQueueEdit { .. }
        | ApplicationOperation::SessionServiceStart { .. }
        | ApplicationOperation::SessionFork { .. }
        | ApplicationOperation::SessionFeedback { .. }
        | ApplicationOperation::SessionCommandFeedback { .. }
        | ApplicationOperation::SessionAgentTeamTaskCreate { .. }
        | ApplicationOperation::SessionAgentTeamTaskReplace { .. }
        | ApplicationOperation::SessionAgentTeamTaskDelete { .. }
        | ApplicationOperation::SessionAgentTeamMessageSend { .. }
        | ApplicationOperation::SessionAgentTeamMessageRead { .. }
        | ApplicationOperation::SessionSkillTurn { .. }
        | ApplicationOperation::AnswerQuestion { .. } => ResourceAction::Submit,
        _ => ResourceAction::ManageExecution,
    }
}

fn submission_display(content: &SubmissionContent) -> String {
    let input = content.input();
    match content.skill_name() {
        None => input.to_owned(),
        Some(name) if input.trim().is_empty() => {
            format!("/skill {name}")
        }
        Some(name) => format!("/skill {name}\n\n{input}"),
    }
}

fn validate_node_snapshot(
    snapshot: &NodeSessionSnapshot,
    expected_workspace: &ternilo_protocol::WorkspaceId,
    expected_session: Option<&SessionId>,
) -> Result<(), HarnessError> {
    snapshot.identity.session_id.validate()?;
    snapshot.workspace_id.validate()?;
    if let Some(parent) = &snapshot.parent_session_id {
        parent.validate()?;
    }
    if snapshot.workspace_id != *expected_workspace
        || expected_session.is_some_and(|session| session != &snapshot.identity.session_id)
    {
        return Err(HarnessError::policy(
            "Node returned a Session outside the resolved placement binding",
        ));
    }
    // Prove the host path was parsed, then deliberately discard it.
    let _ = snapshot.workspace_path.len();
    snapshot.metadata(None).validate()
}

fn translate_reference_ids_from_node(
    mapping: &EdgeSessionRecord,
    mappings: &[EdgeSessionRecord],
    references: &mut [SubmissionReference],
) {
    for reference in references {
        let SubmissionReference::Session { session_id, .. } = reference else {
            continue;
        };
        if let Some(source) = mappings.iter().find(|source| {
            source.executor_id == mapping.executor_id && source.node_session_id == *session_id
        }) {
            session_id.clone_from(&source.session_id);
        }
    }
}

fn decode<T: DeserializeOwned>(value: Value, label: &str) -> Result<T, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::execution(format!("decode {label}: {error}")))
}

fn expose_command_catalog(
    mut catalog: SessionCommandCatalog,
    node_session_id: &SessionId,
    public_session_id: &SessionId,
) -> Result<SessionCommandCatalog, HarnessError> {
    if &catalog.session_id != node_session_id {
        return Err(HarnessError::policy(
            "Node command catalog changed the resolved Session",
        ));
    }
    catalog.session_id.clone_from(public_session_id);
    Ok(catalog)
}

fn decode_exact_submission(
    value: Value,
    expected: &SubmissionId,
    label: &str,
) -> Result<SessionSubmission, HarnessError> {
    let submission: SessionSubmission = decode(value, label)?;
    submission.validate()?;
    if &submission.id != expected {
        return Err(HarnessError::policy(
            "Node Queue response changed the requested submission occurrence",
        ));
    }
    Ok(submission)
}

fn validate_node_inbox(
    snapshot: &SessionInboxSnapshot,
    expected_session: &SessionId,
) -> Result<(), HarnessError> {
    snapshot.session_id.validate()?;
    if &snapshot.session_id != expected_session {
        return Err(HarnessError::policy(
            "Node inbox response changed the resolved Session",
        ));
    }
    if let Some(run_id) = &snapshot.active_run_id {
        run_id.validate()?;
    }
    for (index, item) in snapshot.items.iter().enumerate() {
        item.validate()?;
        if snapshot.items[..index]
            .iter()
            .any(|current| current.id == item.id || current.run_id == item.run_id)
        {
            return Err(HarnessError::policy(
                "Node inbox response contains duplicate occurrence identifiers",
            ));
        }
    }
    Ok(())
}

fn edge_workspace_path() -> String {
    "此电脑 / 已绑定 Workspace".to_owned()
}

#[cfg(test)]
mod tests {
    use ternilo_control::EdgeSessionMetadata;
    use ternilo_protocol::{
        PermissionPreset, RunId, SessionMode, SessionSubmission, SubmissionContent,
        SubmissionPlacement, UserId, WorkspaceId,
    };
    use ternilo_transport::ExecutorId;

    use super::*;

    fn submission(id: &str, run_id: &str) -> SessionSubmission {
        SessionSubmission {
            provenance: None,
            id: SubmissionId::new(id),
            run_id: RunId::new(run_id),
            content: SubmissionContent::Prompt {
                input: id.to_owned(),
            },
            references: Vec::new(),
            attachments: Vec::new(),
            placement: SubmissionPlacement::Queued,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn edge_session(public_id: &str, node_id: &str) -> EdgeSessionRecord {
        EdgeSessionRecord {
            tenant_id: TenantId::new("tenant"),
            session_id: SessionId::new(public_id),
            workspace_id: WorkspaceId::new("workspace"),
            executor_id: ExecutorId::new("executor"),
            owner_user_id: UserId::new("user"),
            node_session_id: SessionId::new(node_id),
            metadata: EdgeSessionMetadata {
                server_model: None,
                parent_session_id: None,
                subagent: None,
                title: public_id.to_owned(),
                archived_at_ms: None,
                blank: false,
                permissions: PermissionPreset::WorkspaceWrite,
                model: json!({ "provider": "profile_default" }),
                agent_preset: "standard".to_owned(),
                preset_plugins: Vec::new(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            last_event_seq: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn shared_commands_require_their_actual_capability() {
        for command in [
            "/extension-enable plugin",
            "/extension-disable plugin",
            "/extension-revoke plugin",
        ] {
            assert_eq!(prompt_action(command), ResourceAction::ManageExecution);
        }
        for command in [
            "/extension-mount plugin",
            "/extension-unmount plugin",
            "/exit-plan",
            "/plan",
        ] {
            assert_eq!(prompt_action(command), ResourceAction::Configure);
        }
        for command in [
            "/agent-stop child",
            "/job-kill job",
            "/terminal-close terminal",
            "/schedule-delete schedule",
        ] {
            assert_eq!(prompt_action(command), ResourceAction::Stop);
        }
        assert_eq!(
            prompt_action("Explain /extension-enable"),
            ResourceAction::Submit
        );
        assert_eq!(
            submission_action(&SubmissionContent::Skill {
                name: "review".to_owned(),
                input: "/extension-enable is text to review".to_owned(),
            }),
            ResourceAction::Submit,
        );
        assert_eq!(
            operation_action(&ApplicationOperation::DefaultModelGet),
            ResourceAction::View
        );
        assert_eq!(
            operation_action(&ApplicationOperation::SessionArchive {
                session_id: SessionId::new("session"),
            }),
            ResourceAction::Delete
        );
        assert_eq!(
            operation_action(&ApplicationOperation::SessionRestore {
                session_id: SessionId::new("session"),
            }),
            ResourceAction::Delete
        );
    }

    #[test]
    fn command_catalog_exposes_the_public_session_id() {
        let commands = vec![ternilo_protocol::CommandDescriptor {
            name: "write".to_owned(),
            description: "Write a file".to_owned(),
            input: None,
        }];
        let catalog = expose_command_catalog(
            SessionCommandCatalog {
                session_id: SessionId::new("node-session"),
                commands: commands.clone(),
            },
            &SessionId::new("node-session"),
            &SessionId::new("public-session"),
        )
        .unwrap();

        assert_eq!(catalog.session_id, SessionId::new("public-session"));
        assert_eq!(catalog.commands, commands);
    }

    #[test]
    fn node_inbox_boundary_rejects_route_spoofing_and_duplicate_occurrences() {
        let expected = SessionId::new("node-session");
        let mut snapshot = SessionInboxSnapshot {
            session_id: expected.clone(),
            active_run_id: None,
            paused: false,
            error: None,
            items: vec![submission("one", "run-one")],
        };
        validate_node_inbox(&snapshot, &expected).unwrap();

        snapshot.session_id = SessionId::new("another-session");
        assert!(validate_node_inbox(&snapshot, &expected).is_err());
        snapshot.session_id = expected.clone();
        snapshot.items.push(submission("one", "run-two"));
        assert!(validate_node_inbox(&snapshot, &expected).is_err());
    }

    #[test]
    fn skill_submission_display_matches_the_node_projection() {
        assert_eq!(
            submission_display(&SubmissionContent::Skill {
                name: "review".to_owned(),
                input: "check this".to_owned(),
            }),
            "/skill review\n\ncheck this"
        );
    }

    #[test]
    fn node_session_references_return_to_the_browser_with_public_ids() {
        let target = edge_session("public-target", "node-target");
        let source = edge_session("public-source", "node-source");
        let mut references = vec![SubmissionReference::Session {
            session_id: SessionId::new("node-source"),
            label: "Source".to_owned(),
        }];
        translate_reference_ids_from_node(&target, &[source], &mut references);
        assert!(matches!(
            &references[0],
            SubmissionReference::Session { session_id, .. }
                if session_id == &SessionId::new("public-source")
        ));
    }
}
