use super::{
    ApplicationOperation, COMMAND_BATCH_LIMIT, COMMAND_DISPATCH_TTL_MS, CommandId, CommandOutcome,
    CommandReply, ConnectedExecutor, ControlFrame, ControlUser, Duration, EdgeGateway,
    ExecutorCommand, ExecutorCommandBody, ExecutorId, HarnessError, InputAuthor, InputProvenance,
    PendingCall, RouteKey, RunId, SessionId, SubmissionId, TenantId, Value, now_ms, oneshot,
};

impl EdgeGateway {
    pub(crate) async fn call(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        operation: ApplicationOperation,
    ) -> Result<Value, HarnessError> {
        self.call_with_timeout(tenant_id, executor_id, operation, Duration::from_mins(30))
            .await
    }

    pub(crate) async fn call_with_timeout(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        operation: ApplicationOperation,
        timeout: Duration,
    ) -> Result<Value, HarnessError> {
        self.call_with_author(tenant_id, executor_id, operation, None, timeout)
            .await
    }

    pub(crate) async fn call_as(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        actor: &ControlUser,
        operation: ApplicationOperation,
    ) -> Result<Value, HarnessError> {
        self.call_with_author(
            tenant_id,
            executor_id,
            operation,
            Some(InputAuthor::Account {
                user_id: actor.user_id.clone(),
                username: actor.username.clone(),
            }),
            Duration::from_mins(30),
        )
        .await
    }

    pub(super) async fn call_with_author(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        operation: ApplicationOperation,
        author: Option<InputAuthor>,
        timeout: Duration,
    ) -> Result<Value, HarnessError> {
        operation.validate()?;
        let route = RouteKey::new(tenant_id.clone(), executor_id.clone());
        self.call_routed(
            route,
            ExecutorCommandBody::Application { request: operation },
            timeout,
            author,
        )
        .await
    }

    pub(crate) async fn cancel_run(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<(), HarnessError> {
        let route = RouteKey::new(tenant_id.clone(), executor_id.clone());
        self.call_routed(
            route,
            ExecutorCommandBody::CancelRun { session_id, run_id },
            Duration::from_mins(30),
            None,
        )
        .await
        .map(|_| ())
    }

    async fn call_routed(
        &self,
        route: RouteKey,
        body: ExecutorCommandBody,
        timeout: Duration,
        author: Option<InputAuthor>,
    ) -> Result<Value, HarnessError> {
        if self.executors.read().await.contains_key(&route) {
            return self.call_body(route, body, timeout, author, None).await;
        }
        let cluster = self
            .cluster
            .as_ref()
            .ok_or_else(|| HarnessError::unavailable("selected Ternilo node is offline"))?;
        let peer = self
            .journal
            .peer_route(&route, now_ms()?)
            .await?
            .filter(|peer| peer.lease.owner_id != self.instance_id)
            .ok_or_else(|| {
                HarnessError::unavailable("selected Ternilo node has no active peer route")
            })?;
        self.store.require_node_credential(&peer.principal).await?;
        let authorization = if creates_input(&body) {
            let Some(InputAuthor::Account { user_id, .. }) = &author else {
                return Err(HarnessError::policy(
                    "Node input requires an authenticated account",
                ));
            };
            Some(
                self.store
                    .node_input_authorization(&peer.principal, user_id)
                    .await?,
            )
        } else {
            None
        };
        cluster
            .forward(peer, route, body, author, timeout, authorization)
            .await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep author admission, durable delivery, waiter cleanup and deletion receipts in one dispatch flow."
    )]
    pub(super) async fn call_body(
        &self,
        route: RouteKey,
        mut body: ExecutorCommandBody,
        timeout: Duration,
        author: Option<InputAuthor>,
        accepted_authorization: Option<ternilo_transport::NodeInputAuthorization>,
    ) -> Result<Value, HarnessError> {
        let connected = self.connected(&route).await?;
        super::forwarding::validate_body(&body, &connected.hello.capabilities)?;
        let deleted_session = match &body {
            ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionDelete { session_id },
            } => Some(session_id.clone()),
            _ => None,
        };
        let now = now_ms()?;
        let command_id = CommandId::new(self.next_identifier("command", now));
        let input_provenance = if creates_input(&body) {
            let input_id = SubmissionId::new(format!("input-{:032x}", rand::random::<u128>()));
            let run_id = bind_input_run(&mut body, &input_id)?;
            Some(InputProvenance {
                input_id,
                run_id: Some(run_id),
                author: author.ok_or_else(|| {
                    HarnessError::policy("Node input requires an authenticated submitter")
                })?,
            })
        } else {
            None
        };
        let input_authorization = match &input_provenance {
            Some(InputProvenance {
                author: InputAuthor::Account { user_id, .. },
                ..
            }) => {
                let current = self
                    .store
                    .node_input_authorization(&connected.principal, user_id)
                    .await?;
                if accepted_authorization
                    .as_ref()
                    .is_some_and(|accepted| accepted != &current)
                {
                    return Err(HarnessError::conflict(
                        "input authority changed during Server peer forwarding",
                    ));
                }
                Some(accepted_authorization.unwrap_or(current))
            }
            _ => None,
        };
        let command = ExecutorCommand {
            command_id,
            scope: connected.scope,
            input_provenance,
            input_authorization,
            issued_at_ms: now,
            expires_at_ms: now.saturating_add(30 * 60 * 1_000),
            body,
        };
        command.validate(now)?;
        let durable = !crate::gateway_journal::requires_ephemeral_delivery(&command.body);
        if durable {
            self.journal.enqueue(&route, &command).await?;
        }
        let command_id = command.command_id.clone();
        let key = (route.tenant_id.clone(), command_id.clone());
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(
            key.clone(),
            PendingCall {
                route: route.clone(),
                connection_id: (!durable).then(|| connected.connection_id.clone()),
                sender,
            },
        );
        let sent = if durable {
            self.dispatch_available(&route).await
        } else {
            connected
                .sender
                .send(ControlFrame::Command {
                    command: Box::new(command),
                })
                .await
                .map_err(|_| HarnessError::unavailable("Ternilo Node connection closed"))
        };
        if let Err(error) = sent {
            if durable {
                eprintln!("Node command {command_id} remains queued: {error}");
            } else {
                self.pending.lock().await.remove(&key);
                return Err(error);
            }
        }
        let reply = self
            .wait_for_reply(&route, &command_id, receiver, durable, timeout)
            .await;
        self.pending.lock().await.remove(&key);
        match reply?.outcome {
            CommandOutcome::Ok { value } => {
                if let Some(session_id) = deleted_session {
                    self.store
                        .delete_events(&route.tenant_id, &route.executor_id, &session_id)
                        .await?;
                }
                Ok(value)
            }
            CommandOutcome::Error { error } => Err(error),
        }
    }

    pub(super) async fn wait_for_reply(
        &self,
        route: &RouteKey,
        command_id: &CommandId,
        mut receiver: oneshot::Receiver<CommandReply>,
        durable: bool,
        timeout: Duration,
    ) -> Result<CommandReply, HarnessError> {
        let wait = async {
            let mut poll = tokio::time::interval(Duration::from_millis(250));
            poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    reply = &mut receiver => return reply.map_err(|_| {
                        HarnessError::unavailable("Ternilo Node connection closed before replying")
                    }),
                    _ = poll.tick(), if durable => {
                        if let Some(reply) = self.journal.reply(route, command_id).await? {
                            return Ok(reply);
                        }
                        self.expire_commands(route).await?;
                    }
                }
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| HarnessError::execution("Ternilo Node command timed out"))?
    }

    pub(super) async fn dispatch_available(&self, route: &RouteKey) -> Result<(), HarnessError> {
        self.expire_commands(route).await?;
        let Some(connected) = self.executors.read().await.get(route).cloned() else {
            return Ok(());
        };
        loop {
            let commands = self
                .journal
                .claim(
                    route,
                    &connected.lease,
                    now_ms()?,
                    COMMAND_DISPATCH_TTL_MS,
                    COMMAND_BATCH_LIMIT,
                )
                .await?;
            let count = commands.len();
            for command in commands {
                connected
                    .sender
                    .send(ControlFrame::Command {
                        command: Box::new(command),
                    })
                    .await
                    .map_err(|_| HarnessError::unavailable("Ternilo Node connection closed"))?;
            }
            if count < COMMAND_BATCH_LIMIT as usize {
                return Ok(());
            }
        }
    }

    pub(super) async fn expire_commands(&self, route: &RouteKey) -> Result<(), HarnessError> {
        for reply in self.journal.expire(route, now_ms()?).await? {
            let key = (route.tenant_id.clone(), reply.command_id.clone());
            if let Some(pending) = self.pending.lock().await.remove(&key) {
                let _ = pending.sender.send(reply);
            }
        }
        Ok(())
    }

    pub(super) async fn accept_reply(
        &self,
        route: &RouteKey,
        connected: &ConnectedExecutor,
        reply: CommandReply,
    ) -> Result<(), HarnessError> {
        let durable = self.journal.contains(route, &reply.command_id).await?;
        if durable {
            self.journal
                .complete(route, &connected.lease, &reply, now_ms()?)
                .await?;
        }
        let key = (route.tenant_id.clone(), reply.command_id.clone());
        let pending = {
            let mut calls = self.pending.lock().await;
            if calls.get(&key).is_some_and(|call| {
                call.route == *route
                    && if durable {
                        call.connection_id.is_none()
                    } else {
                        call.connection_id.as_ref() == Some(&connected.connection_id)
                    }
            }) {
                calls.remove(&key)
            } else {
                None
            }
        };
        if let Some(pending) = pending {
            let _ = pending.sender.send(reply);
        }
        Ok(())
    }
}

pub(super) fn creates_input(body: &ExecutorCommandBody) -> bool {
    matches!(
        body,
        ExecutorCommandBody::Application {
            request: ApplicationOperation::SessionSubmit { .. }
                | ApplicationOperation::SessionTurn { .. }
                | ApplicationOperation::SessionSkillTurn { .. }
                | ApplicationOperation::SessionSubagentFollowup { .. }
        }
    )
}

pub(super) fn bind_input_run(
    body: &mut ExecutorCommandBody,
    input: &SubmissionId,
) -> Result<RunId, HarnessError> {
    let default = RunId::new(format!("run-{}", input.as_str()));
    let run = match body {
        ExecutorCommandBody::Application { request } => match request {
            ApplicationOperation::SessionSubmit { request, .. } => {
                request.run_id.get_or_insert(default).clone()
            }
            ApplicationOperation::SessionTurn { run_id, .. }
            | ApplicationOperation::SessionSkillTurn { run_id, .. } => RunId::new(
                run_id
                    .get_or_insert_with(|| default.as_str().to_owned())
                    .clone(),
            ),
            ApplicationOperation::SessionSubagentFollowup { .. } => default,
            _ => return Err(HarnessError::invalid("operation has no input run")),
        },
        _ => return Err(HarnessError::invalid("operation has no input run")),
    };
    run.validate()?;
    Ok(run)
}
