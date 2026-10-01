use super::{
    AgentInput, Arc, Attachment, HarnessError, InputAuthor, InputProvenance, LocalApplication,
    ManagedSession, PendingQuestion, PreparedTurnInput, RunId, RunOutcome, SessionEventKind,
    SessionMode, SessionSubmissionRequest, SkillCatalogSnapshot, SteeringInput, SubmissionContent,
    SubmissionDelivery, SubmissionId, SubmissionReference, TurnInput, TurnRequest,
    UserMessageSource, now_ms, submissions,
};

impl LocalApplication {
    pub async fn run_turn(
        &self,
        session_id: &str,
        run_id: Option<String>,
        input: String,
    ) -> Result<RunOutcome, HarnessError> {
        self.run_turn_with_attachments(session_id, run_id, input, Vec::new())
            .await
    }

    pub async fn run_turn_with_attachments(
        &self,
        session_id: &str,
        run_id: Option<String>,
        input: String,
        attachments: Vec<Attachment>,
    ) -> Result<RunOutcome, HarnessError> {
        self.run_session_input_with_provenance(
            session_id,
            SessionSubmissionRequest {
                run_id: run_id.map(RunId::new),
                content: SubmissionContent::Prompt { input },
                references: Vec::new(),
                attachments,
                delivery: ternilo_protocol::SubmissionDelivery::Queue,
            },
            self.local_input_provenance()?,
        )
        .await
    }

    pub async fn run_skill_turn(
        &self,
        session_id: &str,
        run_id: Option<String>,
        name: String,
        request: String,
        attachments: Vec<Attachment>,
    ) -> Result<RunOutcome, HarnessError> {
        self.run_session_input_with_provenance(
            session_id,
            SessionSubmissionRequest {
                run_id: run_id.map(RunId::new),
                content: SubmissionContent::Skill {
                    name,
                    input: request,
                },
                references: Vec::new(),
                attachments,
                delivery: ternilo_protocol::SubmissionDelivery::Queue,
            },
            self.local_input_provenance()?,
        )
        .await
    }

    pub(super) fn local_input_provenance(&self) -> Result<InputProvenance, HarnessError> {
        Ok(InputProvenance {
            run_id: None,
            input_id: SubmissionId::new(self.next_id("submission")?),
            author: InputAuthor::Local,
        })
    }

    /// Called by trusted transports after establishing the submitter identity.
    pub async fn run_session_input_with_provenance(
        &self,
        session_id: &str,
        request: SessionSubmissionRequest,
        provenance: InputProvenance,
    ) -> Result<RunOutcome, HarnessError> {
        request.validate()?;
        if request.content.regeneration_target().is_some() {
            return Err(HarnessError::invalid(
                "regeneration must be submitted through the session queue",
            ));
        }
        provenance.validate()?;
        let input = submissions::turn_input(&request.content);
        self.run_turn_input(
            session_id,
            TurnRequest {
                run_id: request.run_id.map(|id| id.as_str().to_owned()),
                input,
                references: request.references,
                attachments: request.attachments,
                source_override: None,
                provenance: Some(provenance),
                require_unpaused_inbox: false,
                additional_submissions: Vec::new(),
            },
        )
        .await
    }

    pub(super) async fn prepare_turn_input(
        managed: &ManagedSession,
        input: TurnInput,
    ) -> Result<PreparedTurnInput, HarnessError> {
        match input {
            TurnInput::Prompt(input) => Ok(PreparedTurnInput {
                model_input: input.clone(),
                display_input: None,
                source: None,
                title_input: input,
            }),
            TurnInput::Skill { name, request } => {
                let skill = managed
                    .harness
                    .skill(name.clone())
                    .await?
                    .ok_or_else(|| HarnessError::invalid(format!("unknown skill {name:?}")))?;
                let prepared = ternilo_builtins::prepare_skill_invocation(&skill, &request)?;
                Ok(PreparedTurnInput {
                    model_input: prepared.model_input,
                    display_input: Some(prepared.display_input.clone()),
                    source: Some(UserMessageSource::SkillInvocation { name }),
                    title_input: prepared.display_input,
                })
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one gated turn orders input resolution, execution, inbox settlement, and title events"
    )]
    pub(super) async fn run_turn_input(
        &self,
        session_id: &str,
        request: TurnRequest,
    ) -> Result<RunOutcome, HarnessError> {
        let TurnRequest {
            run_id,
            input,
            references,
            attachments,
            source_override,
            provenance,
            require_unpaused_inbox,
            additional_submissions,
        } = request;
        self.stopping.check()?;
        self.account_authorizations
            .check(provenance.as_ref())
            .await?;
        self.validate_device_input(session_id, provenance.as_ref())
            .await?;
        self.validate_turn_model(session_id, &input).await?;
        let attachments = self.attachments.save_many(attachments).await?;
        let lifecycle = self.session_lifecycle(session_id).await;
        let lifecycle_guard = lifecycle.lock().await;
        let mut managed = self.ensure_session_locked(session_id).await?;
        let mut gate = Arc::clone(&managed.gate).lock_owned().await;
        self.stopping.check()?;
        self.account_authorizations
            .check(provenance.as_ref())
            .await?;
        if self
            .runtime_extensions
            .take_restart_request(session_id)
            .await
        {
            drop(gate);
            self.live.write().await.remove(session_id);
            managed.harness.shutdown().await?;
            managed = self.ensure_session_locked(session_id).await?;
            gate = Arc::clone(&managed.gate).lock_owned().await;
        }
        if require_unpaused_inbox && self.inbox.snapshot(session_id).await?.paused {
            return Err(HarnessError::cancelled("session queue is paused"));
        }
        self.state
            .mark_session_started(session_id, now_ms()?)
            .await?;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Activity,
            None,
        );
        drop(lifecycle_guard);
        let mut prepared = Self::prepare_turn_input(&managed, input).await?;
        let mut additional_inputs = Vec::with_capacity(additional_submissions.len());
        for submission in additional_submissions {
            if let Err(error) = self
                .account_authorizations
                .check(submission.provenance.as_ref())
                .await
            {
                if error.code == ternilo_protocol::ErrorCode::PolicyDenied {
                    self.inbox.finish(session_id, &submission.id).await?;
                    continue;
                }
                return Err(error);
            }
            self.validate_device_input(session_id, submission.provenance.as_ref())
                .await?;
            let prepared =
                Self::prepare_turn_input(&managed, submissions::turn_input(&submission.content))
                    .await?;
            additional_inputs.push(SteeringInput {
                submission_id: submission.id.clone(),
                input: prepared.model_input,
                display_input: prepared.display_input,
                source: submissions::submission_source(&submission, SubmissionDelivery::Queue),
                provenance: submission.provenance,
                references: submission.references,
                reference_contexts: Vec::new(),
                attachments: submission.attachments,
            });
        }
        if let Some(source) = source_override {
            prepared.source = Some(source);
        }
        let run_id = RunId::new(run_id.unwrap_or(self.next_id("run")?));
        run_id.validate()?;
        if prepared.model_input.trim().is_empty() && attachments.is_empty() && references.is_empty()
        {
            return Err(HarnessError::invalid(
                "turn must contain text, an attachment, or a reference",
            ));
        }
        if prepared.title_input.trim().is_empty()
            && let Some(attachment) = attachments.first()
        {
            prepared.title_input.clone_from(&attachment.name);
        }
        if prepared.title_input.trim().is_empty()
            && let Some(reference) = references.first()
        {
            prepared.title_input = match reference {
                SubmissionReference::File { path, .. } => path.clone(),
                SubmissionReference::Session { label, .. } => label.clone(),
            };
        }
        self.stopping.check()?;
        let outcome = managed
            .harness
            .run_input(AgentInput {
                run_id: run_id.clone(),
                additional_inputs,
                input: prepared.model_input,
                display_input: prepared.display_input,
                source: prepared.source,
                provenance,
                references,
                reference_contexts: Vec::new(),
                attachments,
            })
            .await;
        let current_events = managed.harness.events().await;
        let revoked = self
            .model_input_origin(session_id, &run_id)
            .await
            .ok()
            .map(|origin| origin.provenance);
        let revoked = match revoked {
            Some(provenance) => self
                .account_authorizations
                .check(provenance.as_ref())
                .await
                .is_err_and(|error| error.code == ternilo_protocol::ErrorCode::PolicyDenied),
            None => false,
        };
        self.settle_session_inbox(
            session_id,
            &current_events,
            outcome.as_ref().is_err_and(HarnessError::is_cancelled) && !revoked,
        )
        .await?;
        let mut outcome = outcome?;
        let current = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        let settled_at_ms = now_ms()?;
        self.state
            .touch_session(session_id, None, settled_at_ms)
            .await?;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Activity,
            None,
        );
        let first_successful_turn = current_events
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::TurnFinished { .. }))
            .count()
            == 1;
        let direct_command_turn = outcome.events.iter().any(|event| {
            matches!(
                event.kind,
                SessionEventKind::CommandStarted { .. } | SessionEventKind::CommandFinished { .. }
            )
        });
        let model_turn = outcome
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::ModelRequestStarted { .. }));
        if current.title == "New session"
            && first_successful_turn
            && (!direct_command_turn || model_turn)
        {
            if let Ok(event) = managed
                .harness
                .append_event(
                    run_id.clone(),
                    SessionEventKind::SessionTitleGenerationStarted,
                )
                .await
            {
                outcome.events.push(event);
            }
            let mut generated = false;
            let title = tokio::select! {
                biased;
                () = self.stopping.cancelled() => {
                    Err(HarnessError::cancelled("local application is shutting down"))
                }
                title = managed.harness.generate_session_title(
                    run_id.clone(),
                    &prepared.title_input,
                    &outcome.answer,
                ) => title,
            };
            if let Ok(title) = title
                && self
                    .state
                    .set_generated_title(session_id, title.clone(), settled_at_ms)
                    .await
                    .unwrap_or(false)
            {
                self.invalidate(
                    Some(session_id),
                    crate::LocalInvalidationCategory::Workbench,
                    None,
                );
                self.agent_team_provider(session_id)?
                    .invalidate_current_team(None)
                    .await;
                if let Ok(event) = managed
                    .harness
                    .append_event(
                        run_id.clone(),
                        SessionEventKind::SessionTitleGenerated {
                            title: title.clone(),
                        },
                    )
                    .await
                {
                    outcome.events.push(event);
                }
                outcome.generated_title = Some(title);
                generated = true;
            }
            if let Ok(event) = managed
                .harness
                .append_event(
                    run_id,
                    SessionEventKind::SessionTitleGenerationFinished { generated },
                )
                .await
            {
                outcome.events.push(event);
            }
        }
        let approved_plan_exit = managed.boot_mode == SessionMode::Plan
            && outcome.events.iter().any(|event| {
                matches!(
                    event.kind,
                    SessionEventKind::PlanReviewCompleted { approved: true, .. }
                )
            });
        let transitioned = if approved_plan_exit {
            self.state
                .transition_mode(
                    session_id,
                    SessionMode::Plan,
                    SessionMode::Execute,
                    now_ms()?,
                )
                .await?
        } else {
            false
        };
        if transitioned {
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
        }
        let retired = if transitioned {
            let mut live = self.live.write().await;
            if live
                .get(session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &managed))
            {
                live.remove(session_id)
            } else {
                None
            }
        } else {
            None
        };
        drop(gate);
        if let Some(retired) = retired {
            retired.harness.shutdown().await?;
        }
        self.checkpoint_session_fail_soft(session_id).await;
        Ok(outcome)
    }

    pub async fn skill_catalog(
        &self,
        session_id: &str,
    ) -> Result<SkillCatalogSnapshot, HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let managed = self.ensure_session_locked(session_id).await?;
        managed.harness.skill_catalog().await
    }

    pub async fn prepare_skill_invocation(
        &self,
        session_id: &str,
        name: String,
        request: String,
    ) -> Result<ternilo_protocol::PreparedSkillInvocation, HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let managed = self.ensure_session_locked(session_id).await?;
        let skill = managed
            .harness
            .skill(name.clone())
            .await?
            .ok_or_else(|| HarnessError::invalid(format!("unknown skill {name:?}")))?;
        ternilo_builtins::prepare_skill_invocation(&skill, &request)
    }

    pub async fn cancel_turn(&self, session_id: &str, run_id: &str) -> Result<(), HarnessError> {
        self.cancel_turn_inner(session_id, run_id, true).await
    }

    pub(super) async fn cancel_turn_for_queue_restart(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<(), HarnessError> {
        self.cancel_turn_inner(session_id, run_id, false).await
    }

    async fn cancel_turn_inner(
        &self,
        session_id: &str,
        run_id: &str,
        park_queue: bool,
    ) -> Result<(), HarnessError> {
        let run_id = RunId::new(run_id);
        run_id.validate()?;
        let managed = self
            .live
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                HarnessError::invalid(format!(
                    "session {session_id:?} has no active runtime to cancel"
                ))
            })?;
        let active = managed.harness.active_run().await.as_ref() == Some(&run_id);
        if active && park_queue {
            // Park the durable FIFO before signalling cancellation so a waiting
            // driver cannot acquire the released turn gate and consume an item.
            self.inbox.pause(session_id).await?;
        }
        managed.harness.cancel(run_id).await?;
        if !active && park_queue {
            self.inbox.pause(session_id).await?;
        }
        self.interaction.cancel_session(session_id).await;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Questions,
            None,
        );
        Ok(())
    }

    pub async fn pending_questions(&self, session_id: Option<&str>) -> Vec<PendingQuestion> {
        self.interaction.pending(session_id).await
    }

    pub async fn answer_question(
        &self,
        answer: ternilo_protocol::UserAnswer,
    ) -> Result<(), HarnessError> {
        let session_id = self.interaction.answer(answer).await?;
        self.invalidate(
            Some(session_id.as_str()),
            crate::LocalInvalidationCategory::Questions,
            None,
        );
        Ok(())
    }
}
