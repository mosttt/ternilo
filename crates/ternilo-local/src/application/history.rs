use super::workspaces::workspace_title;
use super::{
    AgentId, Arc, BTreeMap, FeedbackRating, HarnessError, JsonlEventStore, LocalApplication,
    LocalSession, Path, Profile, ReferenceCandidate, ReferenceCandidateRequest,
    ReferenceCandidateSnapshot, RunId, SessionArchive, SessionCommandCatalog,
    SessionCommandOutcome, SessionCommandOutcomeKind, SessionCommandReceipt, SessionEvent,
    SessionEventKind, SessionEventReadRequest, SessionExport, SessionId, SessionIdentity,
    SessionProjectionSnapshot, SessionSearchHit, SessionSearchRequest, SessionStats,
    SessionTelemetrySharingStatus, SessionTrace, TenantId, UserId, Workspace, now_ms,
    resolve_named_provider, session_profile, workspace_reference_candidates,
};

impl LocalApplication {
    pub async fn history(
        &self,
        session_id: &str,
        query: ternilo_protocol::SessionHistoryQuery,
        archived_only: bool,
    ) -> Result<ternilo_protocol::SessionEventPage, HarnessError> {
        query.validate()?;
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        if archived_only && session.archived_at_ms.is_none() {
            return Err(HarnessError::conflict("session is no longer archived"));
        }
        if let Some(managed) = self.live.read().await.get(session_id).cloned() {
            return managed.harness.history(query).await;
        }
        JsonlEventStore::new(&self.state.sessions_dir(), &session.identity.session_id)
            .history(query)
            .await
    }

    pub async fn events(&self, session_id: &str) -> Result<Vec<SessionEvent>, HarnessError> {
        self.read_session_event_snapshot(session_id, false).await
    }

    pub async fn archived_events(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        self.read_session_event_snapshot(session_id, true).await
    }

    async fn read_session_event_snapshot(
        &self,
        session_id: &str,
        archived_only: bool,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        if archived_only && session.archived_at_ms.is_none() {
            return Err(HarnessError::conflict("session is no longer archived"));
        }
        let managed = self.live.read().await.get(session_id).cloned();
        if let Some(managed) = managed {
            return Ok(managed.harness.events().await);
        }
        JsonlEventStore::new(&self.state.sessions_dir(), &session.identity.session_id)
            .load_events()
            .await
    }

    pub async fn events_after(
        &self,
        session_id: &str,
        after_seq: Option<u64>,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        let managed = self.live.read().await.get(session_id).cloned();
        if let Some(managed) = managed {
            return Ok(managed.harness.events_after(after_seq).await);
        }
        let start_seq = after_seq.map_or(0, |seq| seq.saturating_add(1));
        Ok(
            JsonlEventStore::new(&self.state.sessions_dir(), &session.identity.session_id)
                .load_from(start_seq)
                .await?
                .events,
        )
    }

    pub async fn search_sessions(
        &self,
        request: SessionSearchRequest,
    ) -> Result<Vec<SessionSearchHit>, HarnessError> {
        self.session_archive
            .search(local_archive_identity(), request)
            .await
    }

    pub async fn read_session_events(
        &self,
        request: SessionEventReadRequest,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        self.session_archive
            .read_events(local_archive_identity(), request)
            .await
    }

    pub async fn reference_candidates(
        &self,
        session_id: &str,
        request: ReferenceCandidateRequest,
    ) -> Result<ReferenceCandidateSnapshot, HarnessError> {
        let current = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        let mut snapshot =
            workspace_reference_candidates(Path::new(&current.workspace_path), &request).await?;
        if !request.directory.is_empty() {
            return Ok(snapshot);
        }
        let needle = request.query.to_lowercase();
        let mut sessions = self
            .state
            .snapshot()
            .await
            .sessions
            .into_iter()
            .filter(|session| {
                session.identity.session_id.as_str() != session_id
                    && session.archived_at_ms.is_none()
                    && (needle.is_empty()
                        || session.title.to_lowercase().contains(&needle)
                        || session
                            .identity
                            .session_id
                            .as_str()
                            .to_lowercase()
                            .contains(&needle))
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            let left_workspace = left.workspace_id == current.workspace_id;
            let right_workspace = right.workspace_id == current.workspace_id;
            right_workspace
                .cmp(&left_workspace)
                .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
        });
        snapshot
            .candidates
            .extend(
                sessions
                    .into_iter()
                    .take(20)
                    .map(|session| ReferenceCandidate::Session {
                        session_id: session.identity.session_id,
                        label: session.title,
                        workspace: session.workspace_path,
                        same_workspace: session.workspace_id == current.workspace_id,
                        updated_at_ms: session.updated_at_ms,
                    }),
            );
        Ok(snapshot)
    }

    pub async fn trace_session(&self, session_id: SessionId) -> Result<SessionTrace, HarnessError> {
        self.session_archive
            .trace(local_archive_identity(), session_id)
            .await
    }

    pub async fn session_projection(
        &self,
        session_id: SessionId,
    ) -> Result<SessionProjectionSnapshot, HarnessError> {
        let session = self
            .state
            .session(session_id.as_str())
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        let units = self.projection_units(&session).await?;
        self.session_archive
            .projection(local_archive_identity(), session_id, units)
            .await
    }

    pub async fn effective_session_profile(
        &self,
        session_id: &str,
    ) -> Result<Profile, HarnessError> {
        let session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        let named_provider = resolve_named_provider(&session.model, &self.providers).await?;
        Ok(session_profile(
            &self.profile,
            &session.model,
            session.server_model.as_ref(),
            named_provider.as_ref(),
            &session.preset_plugins,
            &session.profile_plugins,
            session.mode,
        ))
    }

    pub async fn session_command_catalog(
        &self,
        session_id: &str,
    ) -> Result<SessionCommandCatalog, HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let managed = self.ensure_session_locked(session_id).await?;
        let commands = managed
            .harness
            .command_catalog()
            .await?
            .into_iter()
            .filter(|command| !self.policy.denied_tools.contains(&command.tool_name))
            .map(|command| command.descriptor)
            .collect();
        Ok(SessionCommandCatalog {
            session_id: SessionId::new(session_id),
            commands,
        })
    }

    pub async fn session_telemetry_sharing(
        &self,
        session_id: &str,
    ) -> Result<SessionTelemetrySharingStatus, HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let managed = self.ensure_session_locked(session_id).await?;
        Ok(managed.telemetry.sharing_status())
    }

    pub async fn stats(&self, session_id: &str) -> Result<SessionStats, HarnessError> {
        let projection = self.session_projection(SessionId::new(session_id)).await?;
        serde_json::from_value(
            projection
                .values
                .get("stats")
                .cloned()
                .ok_or_else(|| HarnessError::execution("stats projection is not registered"))?,
        )
        .map_err(|error| HarnessError::execution(format!("decode stats projection: {error}")))
    }

    pub async fn export_session(&self, session_id: &str) -> Result<SessionExport, HarnessError> {
        let session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        let workspace = self
            .state
            .workspace(&session.workspace_id)
            .await
            .unwrap_or_else(|| {
                let path = Path::new(&session.workspace_path);
                Workspace {
                    workspace_id: session.workspace_id.clone(),
                    path: session.workspace_path.clone(),
                    title: workspace_title(path),
                    created_at_ms: session.created_at_ms,
                    updated_at_ms: session.updated_at_ms,
                }
            });
        let events = self.events(session_id).await?;
        Ok(SessionExport {
            schema_version: 1,
            session,
            workspace,
            events,
        })
    }

    pub async fn record_feedback(
        &self,
        session_id: &str,
        target_seq: u64,
        expected_revision: u64,
        rating: Option<FeedbackRating>,
        note: Option<String>,
    ) -> Result<SessionEvent, HarnessError> {
        let note = note
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if note
            .as_ref()
            .is_some_and(|value| value.chars().count() > 8_192)
        {
            return Err(HarnessError::invalid(
                "feedback note must not exceed 8192 characters",
            ));
        }
        let lifecycle = self.session_lifecycle(session_id).await;
        let lifecycle_guard = lifecycle.lock().await;
        let managed = self.ensure_session_locked(session_id).await?;
        let _gate = Arc::clone(&managed.gate).lock_owned().await;
        drop(lifecycle_guard);
        let events = managed.harness.events().await;
        let target = events
            .iter()
            .find(|event| event.seq == target_seq)
            .ok_or_else(|| HarnessError::invalid(format!("unknown event seq {target_seq}")))?;
        if !matches!(target.kind, SessionEventKind::AssistantMessage { .. }) {
            return Err(HarnessError::invalid(
                "feedback target must be an assistant message",
            ));
        }
        let current_revision = events
            .iter()
            .rev()
            .find_map(|event| match event.kind {
                SessionEventKind::FeedbackRecorded {
                    target_seq: recorded_target,
                    revision,
                    ..
                } if recorded_target == target_seq => Some(revision),
                _ => None,
            })
            .unwrap_or(0);
        if current_revision != expected_revision {
            return Err(HarnessError::conflict(format!(
                "feedback revision conflict: expected {expected_revision}, current {current_revision}"
            )));
        }
        managed
            .harness
            .append_event(
                target.run_id.clone(),
                SessionEventKind::FeedbackRecorded {
                    target_seq,
                    revision: current_revision.saturating_add(1),
                    rating,
                    note,
                },
            )
            .await
    }

    /// Record `/feedback` as command/session facts without creating, steering,
    /// or cancelling a model turn. Command input lives only in the dedicated
    /// feedback event; generic command bookkeeping deliberately omits it.
    pub async fn record_command_feedback(
        &self,
        session_id: &str,
        text: String,
    ) -> Result<SessionCommandReceipt, HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let lifecycle_guard = lifecycle.lock().await;
        let managed = self.ensure_session_locked(session_id).await?;
        drop(lifecycle_guard);

        let command_id = self.next_id("feedback")?;
        let run_id = RunId::new(format!("command-{command_id}"));
        run_id.validate()?;
        let started = managed
            .harness
            .append_event(
                run_id.clone(),
                SessionEventKind::CommandStarted {
                    command_id: command_id.clone(),
                    command_name: "feedback".to_owned(),
                },
            )
            .await?;
        let normalized = text.trim();
        let mut events = vec![started];
        let outcome = if normalized.is_empty() {
            SessionCommandOutcome {
                kind: SessionCommandOutcomeKind::Error,
                code: "feedback_text_required".to_owned(),
                parameters: BTreeMap::from([("usage".to_owned(), "/feedback <text>".to_owned())]),
            }
        } else {
            events.push(
                managed
                    .harness
                    .append_event(
                        run_id.clone(),
                        SessionEventKind::FeedbackSubmitted {
                            command_id: command_id.clone(),
                            text: normalized.to_owned(),
                        },
                    )
                    .await?,
            );
            SessionCommandOutcome {
                kind: SessionCommandOutcomeKind::Success,
                code: "feedback_recorded".to_owned(),
                parameters: BTreeMap::from([
                    ("session_id".to_owned(), session_id.to_owned()),
                    (
                        "sharing".to_owned(),
                        telemetry_sharing_name(managed.telemetry.sharing_status()).to_owned(),
                    ),
                ]),
            }
        };
        events.push(
            managed
                .harness
                .append_event(
                    run_id,
                    SessionEventKind::CommandFinished {
                        command_id: command_id.clone(),
                        outcome,
                    },
                )
                .await?,
        );
        self.state
            .touch_session(session_id, None, now_ms()?)
            .await?;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Activity,
            None,
        );
        self.checkpoint_session_fail_soft(session_id).await;
        Ok(SessionCommandReceipt { command_id, events })
    }

    pub(super) async fn projection_units(
        &self,
        session: &LocalSession,
    ) -> Result<Vec<Arc<dyn ternilo_kernel::SessionProjectionUnit>>, HarnessError> {
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
        self.catalog.projection_units(&profile)
    }

    pub(super) async fn checkpoint_session_fail_soft(&self, session_id: &str) {
        let Some(session) = self.state.session(session_id).await else {
            return;
        };
        let Ok(units) = self.projection_units(&session).await else {
            return;
        };
        self.session_archive
            .checkpoint_fail_soft(&session, units)
            .await;
    }
}

const fn telemetry_sharing_name(status: SessionTelemetrySharingStatus) -> &'static str {
    match status {
        SessionTelemetrySharingStatus::Full => "full",
        SessionTelemetrySharingStatus::FeedbackOnly => "feedback_only",
        SessionTelemetrySharingStatus::Disabled => "disabled",
    }
}

fn local_archive_identity() -> SessionIdentity {
    SessionIdentity {
        tenant_id: TenantId::new("local"),
        user_id: UserId::new("local-user"),
        agent_id: AgentId::new("archive-api"),
        session_id: SessionId::new("archive-api"),
    }
}
