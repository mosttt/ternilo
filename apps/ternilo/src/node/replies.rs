use super::application::handle_application_with_provenance;
use super::{
    ApplicationOperation, Arc, BTreeMap, CommandId, CommandReply, Digest, ExecutorCommand,
    ExecutorCommandBody, ExecutorScope, HarnessError, LocalApplication, Mutex, NodeCommandClaim,
    PathBuf, Sha256, TransportStore, Value, VecDeque, now_ms, oneshot,
};
#[cfg(test)]
mod tests;

const MAX_CACHED_REPLIES: usize = 1_024;
const MAX_DURABLE_REPLIES: u32 = 1_024;

pub(super) struct ReplyCache {
    store: TransportStore,
    state: Mutex<ReplyCacheState>,
}

#[derive(Default)]
struct ReplyCacheState {
    entries: BTreeMap<CommandId, ReplyEntry>,
    completed: VecDeque<CommandId>,
}

enum ReplyEntry {
    Running {
        fingerprint: [u8; 32],
        waiters: Vec<oneshot::Sender<CommandReply>>,
    },
    Completed {
        fingerprint: [u8; 32],
        reply: CommandReply,
    },
}

pub(super) enum ReplyClaim {
    Execute,
    Wait(oneshot::Receiver<CommandReply>),
    Completed(CommandReply),
}

impl ReplyCache {
    pub(super) async fn open(path: PathBuf) -> Result<Self, HarnessError> {
        Ok(Self {
            store: TransportStore::open(path).await?,
            state: Mutex::new(ReplyCacheState::default()),
        })
    }

    #[cfg(test)]
    pub(super) async fn open_in_memory() -> Result<Self, HarnessError> {
        Ok(Self {
            store: TransportStore::open_in_memory().await?,
            state: Mutex::new(ReplyCacheState::default()),
        })
    }

    pub(super) async fn claim(
        &self,
        command: &ExecutorCommand,
    ) -> Result<ReplyClaim, HarnessError> {
        let fingerprint = command_fingerprint(command)?;
        let mut state = self.state.lock().await;
        if let Some(entry) = state.entries.get(&command.command_id) {
            let existing = match entry {
                ReplyEntry::Running { fingerprint, .. }
                | ReplyEntry::Completed { fingerprint, .. } => fingerprint,
            };
            if existing != &fingerprint {
                return Err(HarnessError::policy(
                    "gateway reused a command id with different command content",
                ));
            }
        }
        match state.entries.get_mut(&command.command_id) {
            Some(ReplyEntry::Running { waiters, .. }) => {
                let (sender, receiver) = oneshot::channel();
                waiters.push(sender);
                Ok(ReplyClaim::Wait(receiver))
            }
            Some(ReplyEntry::Completed { reply, .. }) => Ok(ReplyClaim::Completed(reply.clone())),
            None if command_requires_ephemeral_delivery(command) => {
                state.entries.insert(
                    command.command_id.clone(),
                    ReplyEntry::Running {
                        fingerprint,
                        waiters: Vec::new(),
                    },
                );
                Ok(ReplyClaim::Execute)
            }
            None => match self.store.claim_node_command(command, now_ms()?).await? {
                NodeCommandClaim::Execute => {
                    state.entries.insert(
                        command.command_id.clone(),
                        ReplyEntry::Running {
                            fingerprint,
                            waiters: Vec::new(),
                        },
                    );
                    Ok(ReplyClaim::Execute)
                }
                NodeCommandClaim::Completed(reply) => {
                    cache_completed(&mut state, fingerprint, reply.clone());
                    Ok(ReplyClaim::Completed(reply))
                }
                NodeCommandClaim::Indeterminate => {
                    let reply = CommandReply::failure(
                        command.command_id.clone(),
                        now_ms()?,
                        HarnessError::execution(
                            "node restarted while this command was running; it was not replayed because its side effects may already have happened",
                        ),
                    );
                    self.store
                        .complete_node_command(command, &reply, MAX_DURABLE_REPLIES)
                        .await?;
                    cache_completed(&mut state, fingerprint, reply.clone());
                    Ok(ReplyClaim::Completed(reply))
                }
                NodeCommandClaim::Conflict => Err(HarnessError::policy(
                    "gateway reused a command id with different command content",
                )),
            },
        }
    }

    pub(super) async fn complete(
        &self,
        command: &ExecutorCommand,
        reply: CommandReply,
    ) -> CommandReply {
        let reply = if command_requires_ephemeral_delivery(command) {
            reply
        } else {
            match self
                .store
                .complete_node_command(command, &reply, MAX_DURABLE_REPLIES)
                .await
            {
                Ok(()) => reply,
                Err(error) => CommandReply::failure(
                    command.command_id.clone(),
                    now_ms().unwrap_or_default(),
                    HarnessError::execution(format!(
                        "command finished but its durable reply could not be committed: {error}"
                    )),
                ),
            }
        };
        let waiters = {
            let mut state = self.state.lock().await;
            // Repeatable private reads do not need cached reply bodies.
            let previous = if matches!(
                &command.body,
                ExecutorCommandBody::Application {
                    request: ApplicationOperation::SessionFileContent { .. }
                        | ApplicationOperation::WorkspaceLocation { .. }
                        | ApplicationOperation::AgentPresetGet { .. }
                }
            ) {
                state.entries.remove(&reply.command_id)
            } else {
                let previous = state.entries.insert(
                    reply.command_id.clone(),
                    ReplyEntry::Completed {
                        fingerprint: command_fingerprint(command)
                            .expect("previously serialized command"),
                        reply: reply.clone(),
                    },
                );
                remember_completed(&mut state, reply.command_id.clone());
                previous
            };
            match previous {
                Some(ReplyEntry::Running { waiters, .. }) => waiters,
                Some(ReplyEntry::Completed { .. }) | None => Vec::new(),
            }
        };
        for waiter in waiters {
            let _ = waiter.send(reply.clone());
        }
        reply
    }
}

fn command_fingerprint(command: &ExecutorCommand) -> Result<[u8; 32], HarnessError> {
    let payload = serde_json::to_vec(command)
        .map_err(|error| HarnessError::invalid(format!("serialize gateway command: {error}")))?;
    Ok(Sha256::digest(payload).into())
}

fn cache_completed(state: &mut ReplyCacheState, fingerprint: [u8; 32], reply: CommandReply) {
    let command_id = reply.command_id.clone();
    state.entries.insert(
        command_id.clone(),
        ReplyEntry::Completed { fingerprint, reply },
    );
    remember_completed(state, command_id);
}

fn remember_completed(state: &mut ReplyCacheState, command_id: CommandId) {
    state.completed.retain(|existing| existing != &command_id);
    state.completed.push_back(command_id);
    while state.completed.len() > MAX_CACHED_REPLIES {
        if let Some(expired) = state.completed.pop_front() {
            state.entries.remove(&expired);
        }
    }
}

fn command_requires_ephemeral_delivery(command: &ExecutorCommand) -> bool {
    matches!(
        &command.body,
        ExecutorCommandBody::Application { request } if request.requires_ephemeral_delivery()
    )
}

pub(super) async fn execute_command(
    application: Arc<LocalApplication>,
    assigned_scope: &ExecutorScope,
    command: ExecutorCommand,
    replies: &ReplyCache,
) -> CommandReply {
    match replies.claim(&command).await {
        Err(error) => {
            return CommandReply::failure(command.command_id, now_ms().unwrap_or_default(), error);
        }
        Ok(ReplyClaim::Completed(reply)) => return reply,
        Ok(ReplyClaim::Wait(receiver)) => {
            return receiver.await.unwrap_or_else(|_| {
                CommandReply::failure(
                    command.command_id,
                    now_ms().unwrap_or_default(),
                    HarnessError::execution("command owner stopped before caching its reply"),
                )
            });
        }
        Ok(ReplyClaim::Execute) => {}
    }
    let command_id = command.command_id.clone();
    let durable_command = command.clone();
    let result = async {
        let now = now_ms()?;
        command.validate(now)?;
        if &command.scope != assigned_scope {
            return Err(HarnessError::policy(
                "command scope does not match the gateway-assigned node scope",
            ));
        }
        match command.body {
            ExecutorCommandBody::Application { request } => {
                handle_application_with_provenance(
                    Arc::clone(&application),
                    request,
                    command.input_provenance,
                )
                .await
            }
            ExecutorCommandBody::CloudRun { .. } => Err(HarnessError::policy(
                "edge nodes do not accept cloud RunSpec commands",
            )),
            ExecutorCommandBody::CancelRun { session_id, run_id } => {
                application
                    .cancel_turn(session_id.as_str(), run_id.as_str())
                    .await?;
                Ok(Value::Null)
            }
        }
    }
    .await;
    let completed_at_ms = now_ms().unwrap_or_default();
    let reply = match result {
        Ok(value) => CommandReply::success(command_id, completed_at_ms, value),
        Err(error) => CommandReply::failure(command_id, completed_at_ms, error),
    };
    replies.complete(&durable_command, reply).await
}
