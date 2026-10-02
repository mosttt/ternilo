use super::{
    AgentId, Arc, HarnessError, JsonlEventStore, LocalApplication, LocalSession,
    LocalSessionUpdate, ModelSelection, PermissionPreset, PluginEntry, Profile, SessionEventKind,
    SessionId, SessionIdentity, SessionMode, TenantId, UserId, WorkspaceId, compose_profiles,
    now_ms, resolve_named_provider, session_profile, validate_local_profile, validate_model,
};

impl LocalApplication {
    pub async fn create_session(
        &self,
        workspace_id: WorkspaceId,
        session_id: Option<String>,
        agent_id: Option<String>,
    ) -> Result<LocalSession, HarnessError> {
        self.create_session_with_preset(workspace_id, session_id, agent_id, None)
            .await
    }

    pub async fn create_session_with_preset(
        &self,
        workspace_id: WorkspaceId,
        session_id: Option<String>,
        agent_id: Option<String>,
        agent_preset: Option<String>,
    ) -> Result<LocalSession, HarnessError> {
        self.create_session_with_options(workspace_id, session_id, agent_id, agent_preset, None)
            .await
    }

    pub async fn create_session_with_options(
        &self,
        workspace_id: WorkspaceId,
        session_id: Option<String>,
        agent_id: Option<String>,
        agent_preset: Option<String>,
        permissions: Option<PermissionPreset>,
    ) -> Result<LocalSession, HarnessError> {
        let workspace =
            self.state.workspace(&workspace_id).await.ok_or_else(|| {
                HarnessError::invalid(format!("unknown workspace {workspace_id}"))
            })?;
        let agent_preset = match agent_preset {
            Some(id) => id,
            None => self.presets.default_id().await,
        };
        let preset = self.agent_preset(&agent_preset).await?;
        let now = now_ms()?;
        let session_id = match session_id {
            Some(session_id) => session_id,
            None => self.next_id("session")?,
        };
        let lifecycle = self.session_lifecycle(&session_id).await;
        let _lifecycle = lifecycle.lock().await;
        self.inbox.require_live_session_id(&session_id).await?;
        let identity = SessionIdentity {
            tenant_id: TenantId::new("local"),
            user_id: UserId::new("local-user"),
            agent_id: AgentId::new(agent_id.unwrap_or_else(|| agent_preset.clone())),
            session_id: SessionId::new(session_id),
        };
        identity.validate()?;
        let inherited_profile = compose_profiles([
            self.profile.clone(),
            Profile {
                plugins: preset.profile.plugins.clone(),
            },
        ]);
        let profile_plugins = crate::extensions::unavailable_inherited_extension_shadows(
            &inherited_profile,
            &self.extension_registry,
        )?;
        let session = LocalSession {
            identity,
            workspace_id: workspace.workspace_id,
            workspace_path: workspace.path,
            parent_session_id: None,
            subagent: None,
            title: "New session".to_owned(),
            archived_at_ms: None,
            blank: true,
            permissions: permissions.unwrap_or(PermissionPreset::WorkspaceWrite),
            model: self.preferences.default_model().await?,
            server_model: None,
            agent_preset,
            preset_plugins: preset.profile.plugins,
            profile_plugins,
            mode: SessionMode::Execute,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.inbox.register_execution_scope(&session).await?;
        self.state.insert_session(session.clone()).await?;
        self.invalidate(
            Some(session.identity.session_id.as_str()),
            crate::LocalInvalidationCategory::Workbench,
            None,
        );
        Ok(session)
    }

    pub async fn fork_session(
        &self,
        parent_session_id: &str,
        session_id: Option<String>,
        agent_id: Option<String>,
    ) -> Result<LocalSession, HarnessError> {
        self.fork_session_at(parent_session_id, None, session_id, agent_id)
            .await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep destination lifecycle checks, seeded history and retained upload facts in one fork operation."
    )]
    pub async fn fork_session_at(
        &self,
        parent_session_id: &str,
        at_seq: Option<u64>,
        session_id: Option<String>,
        agent_id: Option<String>,
    ) -> Result<LocalSession, HarnessError> {
        let lifecycle = self.session_lifecycle(parent_session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let parent = self.state.session(parent_session_id).await.ok_or_else(|| {
            HarnessError::invalid(format!("unknown parent session {parent_session_id:?}"))
        })?;
        let managed = self.live.read().await.get(parent_session_id).cloned();
        let _gate = if let Some(managed) = managed {
            Some(Arc::clone(&managed.gate).lock_owned().await)
        } else {
            None
        };
        let events = JsonlEventStore::new(&self.state.sessions_dir(), &parent.identity.session_id)
            .load_events()
            .await?;
        let boundary = match at_seq {
            Some(at_seq) => events
                .iter()
                .position(|event| event.seq >= at_seq && is_terminal_turn_event(&event.kind)),
            None => events
                .iter()
                .rposition(|event| is_terminal_turn_event(&event.kind)),
        }
        .ok_or_else(|| match at_seq {
            Some(at_seq) => HarnessError::invalid(format!(
                "session {parent_session_id:?} has not completed the turn containing event {at_seq}"
            )),
            None => HarnessError::invalid(format!(
                "session {parent_session_id:?} has no completed turn to fork"
            )),
        })?;
        let seed = &events[..=boundary];
        let child_id = session_id.unwrap_or(self.next_id("session")?);
        if self.state.session(&child_id).await.is_some() {
            return Err(HarnessError::invalid("session already exists"));
        }
        let child_lifecycle = self.session_lifecycle(&child_id).await;
        let _child_lifecycle = child_lifecycle.lock().await;
        self.inbox.require_live_session_id(&child_id).await?;
        if self.state.session(&child_id).await.is_some() {
            return Err(HarnessError::invalid("session already exists"));
        }
        let now = now_ms()?;
        let identity = SessionIdentity {
            tenant_id: parent.identity.tenant_id.clone(),
            user_id: parent.identity.user_id.clone(),
            agent_id: AgentId::new(
                agent_id.unwrap_or_else(|| parent.identity.agent_id.as_str().to_owned()),
            ),
            session_id: SessionId::new(child_id.clone()),
        };
        identity.validate()?;
        let child = LocalSession {
            identity,
            workspace_id: parent.workspace_id.clone(),
            workspace_path: parent.workspace_path.clone(),
            parent_session_id: Some(parent.identity.session_id.clone()),
            subagent: None,
            title: increased_fork_title(&parent.title),
            archived_at_ms: None,
            blank: false,
            permissions: parent.permissions,
            model: parent.model,
            server_model: parent.server_model,
            agent_preset: parent.agent_preset,
            preset_plugins: parent.preset_plugins,
            profile_plugins: parent.profile_plugins,
            mode: parent.mode,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.inbox.register_execution_scope(&child).await?;
        let store = JsonlEventStore::new(&self.state.sessions_dir(), &child.identity.session_id);
        store.seed_events(seed).await?;
        if let Err(error) = self.state.insert_forked_session(child.clone()).await {
            let _ = store.remove().await;
            return Err(error);
        }
        let inherited_ids = seed
            .iter()
            .flat_map(ternilo_protocol::session_file_references)
            .map(|file| file.id)
            .collect::<std::collections::BTreeSet<_>>();
        let uploads = self
            .inbox
            .accepted_uploads(parent_session_id)
            .await?
            .into_iter()
            .filter(|upload| inherited_ids.contains(&upload.file_id()))
            .collect();
        self.inbox.retain_uploads(&child_id, uploads).await?;
        let index = self.session_archive.index();
        index
            .append_many_fail_soft(
                child.identity.session_id.as_str().to_owned(),
                child.workspace_id.as_str().to_owned(),
                seed,
            )
            .await;
        // Projection rows are a rebuildable read cache. The Web client asks
        // for them after opening the child, so keep that work off the fork's
        // user-visible critical path instead of projecting the full history
        // before the POST can return.
        self.invalidate(
            Some(child.identity.session_id.as_str()),
            crate::LocalInvalidationCategory::Workbench,
            None,
        );
        Ok(child)
    }

    pub async fn update_session(
        &self,
        session_id: &str,
        update: LocalSessionUpdate,
    ) -> Result<LocalSession, HarnessError> {
        if update.is_empty() {
            return Err(HarnessError::invalid("session update has no fields"));
        }
        let changes_team_label = update.title.is_some();
        let changes_preset = update.agent_preset.is_some();
        let restarts_runtime = update.restarts_runtime();
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let mut session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        if changes_preset
            && (!session.blank || !self.inbox.snapshot(session_id).await?.items.is_empty())
        {
            return Err(HarnessError::conflict(
                "agent preset is locked after the first accepted task; create a new session to use another preset",
            ));
        }
        if let Some(title) = update.title {
            let title = title.trim();
            if title.is_empty() || title.chars().count() > 120 {
                return Err(HarnessError::invalid(
                    "session title must contain 1 to 120 characters",
                ));
            }
            title.clone_into(&mut session.title);
        }
        if let Some(permissions) = update.permissions {
            session.permissions = permissions;
        }
        if let Some(model) = update.model {
            validate_model(&model)?;
            if matches!(
                model,
                ModelSelection::AccountProvider { .. }
                    | ModelSelection::PlatformModel { .. }
                    | ModelSelection::ComputerProvider { .. }
            ) {
                let snapshot = update.server_model.ok_or_else(|| {
                    HarnessError::policy("Server models must be selected through their Server")
                })?;
                snapshot.validate()?;
                session.server_model = Some(snapshot);
            } else {
                session.server_model = None;
            }
            session.model = model;
        }
        if let Some(agent_preset) = update.agent_preset {
            let preset = self.agent_preset(&agent_preset).await?;
            session.agent_preset = agent_preset;
            session.preset_plugins = preset.profile.plugins;
        }
        if let Some(profile_plugins) = update.profile_plugins {
            session.profile_plugins = profile_plugins;
        }
        if let Some(mode) = update.mode {
            session.mode = mode;
        }
        let named_provider = resolve_named_provider(&session.model, &self.providers).await?;
        let candidate = session_profile(
            &self.profile,
            &session.model,
            session.server_model.as_ref(),
            named_provider.as_ref(),
            &session.preset_plugins,
            &session.profile_plugins,
            session.mode,
        );
        validate_local_profile(&candidate, &self.catalog, &self.extension_registry)?;
        if restarts_runtime {
            self.stop_live_session(session_id).await?;
        }
        session.updated_at_ms = now_ms()?;
        let session = self.state.replace_session(session_id, session).await?;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Profile,
            None,
        );
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Workbench,
            None,
        );
        if changes_team_label {
            self.agent_team_provider(session_id)?
                .invalidate_current_team(None)
                .await;
        }
        Ok(session)
    }

    pub async fn update_permissions(
        &self,
        session_id: &str,
        permissions: PermissionPreset,
    ) -> Result<LocalSession, HarnessError> {
        self.update_session(
            session_id,
            LocalSessionUpdate {
                permissions: Some(permissions),
                ..LocalSessionUpdate::default()
            },
        )
        .await
    }

    pub async fn update_model(
        &self,
        session_id: &str,
        model: ModelSelection,
    ) -> Result<LocalSession, HarnessError> {
        self.update_session(
            session_id,
            LocalSessionUpdate {
                model: Some(model),
                ..LocalSessionUpdate::default()
            },
        )
        .await
    }

    pub async fn update_profile_plugins(
        &self,
        session_id: &str,
        profile_plugins: Vec<PluginEntry>,
    ) -> Result<LocalSession, HarnessError> {
        self.update_session(
            session_id,
            LocalSessionUpdate {
                profile_plugins: Some(profile_plugins),
                ..LocalSessionUpdate::default()
            },
        )
        .await
    }

    pub async fn update_mode(
        &self,
        session_id: &str,
        mode: SessionMode,
    ) -> Result<LocalSession, HarnessError> {
        self.update_session(
            session_id,
            LocalSessionUpdate {
                mode: Some(mode),
                ..LocalSessionUpdate::default()
            },
        )
        .await
    }

    pub async fn update_title(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<LocalSession, HarnessError> {
        self.update_session(
            session_id,
            LocalSessionUpdate {
                title: Some(title),
                ..LocalSessionUpdate::default()
            },
        )
        .await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<(), HarnessError> {
        let team_session_ids = self
            .agent_team_provider(session_id)?
            .current_session_ids()
            .await
            .unwrap_or_default();
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        if self.state.session(session_id).await.is_none() {
            return Ok(());
        }
        self.stop_live_session(session_id).await?;
        let session = self.state.delete_session(session_id).await?;
        JsonlEventStore::new(&self.state.sessions_dir(), &session.identity.session_id)
            .remove()
            .await?;
        self.inbox.remove_session(session_id).await?;
        self.session_archive.delete_from_index(session_id).await;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Workbench,
            None,
        );
        for team_session_id in team_session_ids {
            if team_session_id.as_str() != session_id {
                self.invalidate(
                    Some(team_session_id.as_str()),
                    crate::LocalInvalidationCategory::AgentTeam,
                    None,
                );
            }
        }
        Ok(())
    }

    pub async fn restore_session(&self, session_id: &str) -> Result<LocalSession, HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        self.inbox.require_live_session_id(session_id).await?;
        let session = self.state.restore_session(session_id).await?;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Workbench,
            None,
        );
        Ok(session)
    }

    pub async fn archive_session(&self, session_id: &str) -> Result<LocalSession, HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        if self.state.session(session_id).await.is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown session {session_id:?}"
            )));
        }
        self.stop_live_session(session_id).await?;
        let session = self.state.archive_session(session_id, now_ms()?).await?;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Workbench,
            None,
        );
        Ok(session)
    }
}

pub(super) fn is_terminal_turn_event(kind: &SessionEventKind) -> bool {
    matches!(
        kind,
        SessionEventKind::TurnFinished { .. }
            | SessionEventKind::TurnFailed { .. }
            | SessionEventKind::TurnCancelled
    )
}

pub(super) fn increased_fork_title(title: &str) -> String {
    increment_parenthesized_suffix(title, '(', ')')
        .or_else(|| increment_parenthesized_suffix(title, '（', '）'))
        .unwrap_or_else(|| format!("{title} (1)"))
}

fn increment_parenthesized_suffix(title: &str, open: char, close: char) -> Option<String> {
    let body = title.strip_suffix(close)?;
    let open_index = body.rfind(open)?;
    let digits = &body[open_index + open.len_utf8()..];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    let mut incremented = if digits.is_empty() {
        vec![b'1']
    } else {
        digits.as_bytes().to_vec()
    };
    if !digits.is_empty() {
        let mut carry = true;
        for digit in incremented.iter_mut().rev() {
            if *digit == b'9' {
                *digit = b'0';
            } else {
                *digit += 1;
                carry = false;
                break;
            }
        }
        if carry {
            incremented.insert(0, b'1');
        }
    }
    let incremented = String::from_utf8(incremented).expect("ASCII digits are valid UTF-8");
    Some(format!(
        "{}{}{}{}",
        &body[..open_index],
        open,
        incremented,
        close
    ))
}
