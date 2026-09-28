use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::json;
use ternilo_authorization::{
    AuthorizationFlow, AuthorizationInteraction, AuthorizationOutcome, AuthorizationService,
    AuthorizationSession, AuthorizationWriter,
};
use ternilo_protocol::{
    AuthorizationAttempt, AuthorizationAttemptStatus, AuthorizationBeginRequest,
    AuthorizationCredentialKey, AuthorizationCredentialSpace, AuthorizationMethod,
    AuthorizationNotice, AuthorizationPrompt, AuthorizationPromptAnswer, AuthorizationSnapshot,
    HarnessError, PendingAuthorizationNotice, PendingAuthorizationPrompt,
};
use tokio::{sync::oneshot, task::JoinHandle};

const MAX_RETAINED_ATTEMPTS: usize = 64;
const MAX_NOTICES_PER_ATTEMPT: usize = 32;

pub struct LocalAuthorizations {
    service: Arc<AuthorizationService>,
    broker: Arc<AuthorizationBroker>,
    tasks: tokio::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl LocalAuthorizations {
    #[must_use]
    pub fn new(service: Arc<AuthorizationService>) -> Arc<Self> {
        Arc::new(Self {
            service,
            broker: Arc::new(AuthorizationBroker::default()),
            tasks: tokio::sync::Mutex::new(Vec::new()),
        })
    }

    /// Register one plugin-owned interactive sign-in flow.
    ///
    /// Provider API keys deliberately do not use this seam: users enter those directly on the
    /// Models page. This registry is only for credentials that require a human authorization
    /// conversation owned by a plugin (OAuth, device code, account selection, and similar).
    pub async fn register_plugin_flow(
        &self,
        flow: Arc<dyn AuthorizationFlow>,
    ) -> Result<(), HarnessError> {
        self.service.register(flow).await
    }

    /// Register the deterministic device-code flow used by the real browser fixture.
    pub async fn register_fixture_flow(&self) -> Result<(), HarnessError> {
        self.register_plugin_flow(Arc::new(FixtureDeviceCodeFlow))
            .await
    }

    pub async fn snapshot(&self, surface_id: &str) -> Result<AuthorizationSnapshot, HarnessError> {
        validate_surface_id(surface_id)?;
        let (attempts, notices, prompts) = self.broker.snapshot(surface_id);
        Ok(AuthorizationSnapshot {
            entries: self.service.list().await?,
            attempts,
            notices,
            prompts,
        })
    }

    pub async fn begin(
        self: &Arc<Self>,
        request: AuthorizationBeginRequest,
    ) -> Result<AuthorizationAttempt, HarnessError> {
        request.key.validate()?;
        validate_surface_id(&request.surface_id)?;
        let entry = self.service.describe(&request.key).await?.ok_or_else(|| {
            HarnessError::invalid(format!(
                "no authorization flow is registered for {:?}",
                request.key.key
            ))
        })?;
        if entry.in_flight || self.broker.key_is_running(&request.key) {
            return Err(HarnessError::policy(format!(
                "an authorization attempt for {:?} is already running",
                request.key.key
            )));
        }
        if !entry.writable {
            return Err(HarnessError::policy(format!(
                "credential {:?} is read-only and cannot be authorized interactively",
                request.key.key
            )));
        }
        let method = request
            .method
            .unwrap_or_else(|| entry.methods[0].id.clone());
        if !entry.methods.iter().any(|candidate| candidate.id == method) {
            return Err(HarnessError::invalid(format!(
                "authorization flow for {:?} offers no method {method:?}",
                request.key.key
            )));
        }
        let attempt = self.broker.start_attempt(
            request.surface_id.clone(),
            request.key.clone(),
            method.clone(),
        )?;
        let interaction: Arc<dyn AuthorizationInteraction> = Arc::new(BrokerInteraction {
            broker: Arc::clone(&self.broker),
            surface_id: request.surface_id,
            attempt_id: attempt.attempt_id.clone(),
            key: request.key.clone(),
        });
        let manager = Arc::clone(self);
        let attempt_id = attempt.attempt_id.clone();
        let key = request.key;
        let handle = tokio::spawn(async move {
            let result = manager.service.begin(key, Some(method), interaction).await;
            manager.broker.finish_attempt(&attempt_id, result);
        });
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|task| !task.is_finished());
        tasks.push(handle);
        Ok(attempt)
    }

    pub fn answer(&self, answer: AuthorizationPromptAnswer) -> Result<(), HarnessError> {
        validate_surface_id(&answer.surface_id)?;
        if answer.value.is_empty() || answer.value.len() > 64 * 1024 {
            return Err(HarnessError::invalid(
                "authorization prompt answer must contain 1 to 65536 bytes",
            ));
        }
        self.broker.answer(answer)
    }

    pub async fn cancel(&self, key: &AuthorizationCredentialKey) -> Result<(), HarnessError> {
        key.validate()?;
        self.broker.cancel_key(key);
        self.service.cancel(key).await;
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.broker.cancel_all();
        self.service.cancel_all().await;
        let tasks = std::mem::take(&mut *self.tasks.lock().await);
        for task in tasks {
            let _ = task.await;
        }
    }
}

struct FixtureDeviceCodeFlow;

impl FixtureDeviceCodeFlow {
    fn credential_key() -> AuthorizationCredentialKey {
        AuthorizationCredentialKey {
            space: AuthorizationCredentialSpace::Record,
            key: "dev.ternilo.authorization-fixture/device-code".to_owned(),
        }
    }
}

impl AuthorizationFlow for FixtureDeviceCodeFlow {
    fn key(&self) -> AuthorizationCredentialKey {
        Self::credential_key()
    }

    fn label(&self) -> &'static str {
        "Fixture plugin account"
    }

    fn methods(&self) -> Vec<AuthorizationMethod> {
        vec![AuthorizationMethod {
            id: "device-code".to_owned(),
            label: "Sign in with device code".to_owned(),
        }]
    }

    fn run<'a>(
        &'a self,
        session: AuthorizationSession,
        writer: AuthorizationWriter,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            session.notify(AuthorizationNotice {
                message: "Open the device page and enter this one-time code.".to_owned(),
                url: Some("https://example.test/device".to_owned()),
                code: Some("TERNILO-42".to_owned()),
            });
            let answer = session
                .prompt(AuthorizationPrompt::Select {
                    message: "After completing the device page, confirm the result.".to_owned(),
                    options: vec![
                        ternilo_protocol::AuthorizationPromptOption {
                            id: "complete".to_owned(),
                            label: "I completed sign-in".to_owned(),
                            description: Some(
                                "Save the fixture plugin grant on this Ternilo host.".to_owned(),
                            ),
                        },
                        ternilo_protocol::AuthorizationPromptOption {
                            id: "decline".to_owned(),
                            label: "Decline".to_owned(),
                            description: Some("Finish without saving a credential.".to_owned()),
                        },
                    ],
                })
                .await?;
            if answer != "complete" {
                return Err(HarnessError::cancelled(
                    "fixture plugin authorization was declined",
                ));
            }
            writer
                .set_record(
                    "grant".to_owned(),
                    json!({
                        "provider": "fixture-device-code",
                        "account": "fixture-user",
                        "authorized": true,
                    }),
                )
                .await
        })
    }
}

struct OwnedAttempt {
    surface_id: String,
    public: AuthorizationAttempt,
}

struct OwnedNotice {
    surface_id: String,
    public: PendingAuthorizationNotice,
}

struct PendingPrompt {
    surface_id: String,
    public: PendingAuthorizationPrompt,
    sender: oneshot::Sender<String>,
}

#[derive(Default)]
struct BrokerState {
    attempts: BTreeMap<String, OwnedAttempt>,
    notices: BTreeMap<String, OwnedNotice>,
    prompts: BTreeMap<String, PendingPrompt>,
}

#[derive(Default)]
struct AuthorizationBroker {
    state: Mutex<BrokerState>,
    next_id: AtomicU64,
}

impl AuthorizationBroker {
    fn snapshot(
        &self,
        surface_id: &str,
    ) -> (
        Vec<AuthorizationAttempt>,
        Vec<PendingAuthorizationNotice>,
        Vec<PendingAuthorizationPrompt>,
    ) {
        let state = self.state.lock().expect("authorization broker poisoned");
        let mut attempts = state
            .attempts
            .values()
            .filter(|attempt| attempt.surface_id == surface_id)
            .map(|attempt| attempt.public.clone())
            .collect::<Vec<_>>();
        attempts.sort_by_key(|attempt| attempt.started_at_ms);
        let notices = state
            .notices
            .values()
            .filter(|notice| notice.surface_id == surface_id)
            .map(|notice| notice.public.clone())
            .collect();
        let prompts = state
            .prompts
            .values()
            .filter(|prompt| prompt.surface_id == surface_id)
            .map(|prompt| prompt.public.clone())
            .collect();
        (attempts, notices, prompts)
    }

    fn key_is_running(&self, key: &AuthorizationCredentialKey) -> bool {
        self.state
            .lock()
            .expect("authorization broker poisoned")
            .attempts
            .values()
            .any(|attempt| {
                attempt.public.key == *key
                    && attempt.public.status == AuthorizationAttemptStatus::Running
            })
    }

    fn start_attempt(
        &self,
        surface_id: String,
        key: AuthorizationCredentialKey,
        method: String,
    ) -> Result<AuthorizationAttempt, HarnessError> {
        let now = now_ms()?;
        let attempt = AuthorizationAttempt {
            attempt_id: self.next_identifier("authorization", now),
            key,
            method,
            status: AuthorizationAttemptStatus::Running,
            error: None,
            started_at_ms: now,
            updated_at_ms: now,
        };
        let mut state = self.state.lock().expect("authorization broker poisoned");
        prune_attempts(&mut state);
        state.attempts.insert(
            attempt.attempt_id.clone(),
            OwnedAttempt {
                surface_id,
                public: attempt.clone(),
            },
        );
        Ok(attempt)
    }

    fn notify(
        &self,
        surface_id: String,
        attempt_id: String,
        key: AuthorizationCredentialKey,
        notice: AuthorizationNotice,
    ) {
        let now = now_ms().unwrap_or_default();
        let id = self.next_identifier("authorization-notice", now);
        let mut state = self.state.lock().expect("authorization broker poisoned");
        if !attempt_is_running(&state, &attempt_id, &surface_id) {
            return;
        }
        let notice_count = state
            .notices
            .values()
            .filter(|notice| notice.public.attempt_id == attempt_id)
            .count();
        if notice_count >= MAX_NOTICES_PER_ATTEMPT
            && let Some(oldest) = state
                .notices
                .iter()
                .find(|(_, notice)| notice.public.attempt_id == attempt_id)
                .map(|(id, _)| id.clone())
        {
            state.notices.remove(&oldest);
        }
        state.notices.insert(
            id.clone(),
            OwnedNotice {
                surface_id,
                public: PendingAuthorizationNotice {
                    id,
                    attempt_id,
                    key,
                    notice,
                },
            },
        );
    }

    fn prompt(
        &self,
        surface_id: String,
        attempt_id: String,
        key: AuthorizationCredentialKey,
        prompt: AuthorizationPrompt,
    ) -> Result<oneshot::Receiver<String>, HarnessError> {
        let now = now_ms()?;
        let id = self.next_identifier("authorization-prompt", now);
        let (sender, receiver) = oneshot::channel();
        let mut state = self.state.lock().expect("authorization broker poisoned");
        if !attempt_is_running(&state, &attempt_id, &surface_id) {
            return Err(HarnessError::cancelled(
                "authorization interaction is no longer active",
            ));
        }
        state.prompts.insert(
            id.clone(),
            PendingPrompt {
                surface_id,
                public: PendingAuthorizationPrompt {
                    id,
                    attempt_id,
                    key,
                    prompt,
                },
                sender,
            },
        );
        Ok(receiver)
    }

    fn answer(&self, answer: AuthorizationPromptAnswer) -> Result<(), HarnessError> {
        let mut state = self.state.lock().expect("authorization broker poisoned");
        let pending = state.prompts.get(&answer.prompt_id).ok_or_else(|| {
            HarnessError::invalid(format!(
                "unknown or already answered authorization prompt {:?}",
                answer.prompt_id
            ))
        })?;
        if pending.surface_id != answer.surface_id {
            return Err(HarnessError::policy(
                "authorization prompt belongs to a different interaction surface",
            ));
        }
        let pending = state
            .prompts
            .remove(&answer.prompt_id)
            .expect("prompt was checked while holding the broker lock");
        pending.sender.send(answer.value).map_err(|_| {
            HarnessError::execution("authorization flow stopped before receiving the answer")
        })
    }

    fn finish_attempt(&self, attempt_id: &str, result: Result<AuthorizationOutcome, HarnessError>) {
        let mut state = self.state.lock().expect("authorization broker poisoned");
        let Some(attempt) = state.attempts.get_mut(attempt_id) else {
            return;
        };
        let (status, error) = match result {
            Ok(AuthorizationOutcome::Authorized) => (AuthorizationAttemptStatus::Authorized, None),
            Ok(AuthorizationOutcome::Cancelled) => (AuthorizationAttemptStatus::Cancelled, None),
            Err(error) => (AuthorizationAttemptStatus::Failed, Some(error.to_string())),
        };
        attempt.public.status = status;
        attempt.public.error = error;
        attempt.public.updated_at_ms = now_ms().unwrap_or(attempt.public.started_at_ms);
        state
            .prompts
            .retain(|_, prompt| prompt.public.attempt_id != attempt_id);
    }

    fn cancel_key(&self, key: &AuthorizationCredentialKey) {
        let mut state = self.state.lock().expect("authorization broker poisoned");
        let cancelled = state
            .attempts
            .values_mut()
            .filter(|attempt| {
                attempt.public.key == *key
                    && attempt.public.status == AuthorizationAttemptStatus::Running
            })
            .map(|attempt| {
                attempt.public.status = AuthorizationAttemptStatus::Cancelled;
                attempt.public.updated_at_ms = now_ms().unwrap_or(attempt.public.started_at_ms);
                attempt.public.attempt_id.clone()
            })
            .collect::<BTreeSet<_>>();
        state
            .prompts
            .retain(|_, prompt| !cancelled.contains(&prompt.public.attempt_id));
    }

    fn cancel_all(&self) {
        let mut state = self.state.lock().expect("authorization broker poisoned");
        for attempt in state.attempts.values_mut() {
            if attempt.public.status == AuthorizationAttemptStatus::Running {
                attempt.public.status = AuthorizationAttemptStatus::Cancelled;
                attempt.public.updated_at_ms = now_ms().unwrap_or(attempt.public.started_at_ms);
            }
        }
        state.prompts.clear();
    }

    fn next_identifier(&self, prefix: &str, now: u64) -> String {
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{now}-{sequence}")
    }
}

struct BrokerInteraction {
    broker: Arc<AuthorizationBroker>,
    surface_id: String,
    attempt_id: String,
    key: AuthorizationCredentialKey,
}

impl AuthorizationInteraction for BrokerInteraction {
    fn notify(&self, notice: AuthorizationNotice) {
        self.broker.notify(
            self.surface_id.clone(),
            self.attempt_id.clone(),
            self.key.clone(),
            notice,
        );
    }

    fn prompt<'a>(
        &'a self,
        prompt: AuthorizationPrompt,
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.broker
                .prompt(
                    self.surface_id.clone(),
                    self.attempt_id.clone(),
                    self.key.clone(),
                    prompt,
                )?
                .await
                .map_err(|_| HarnessError::cancelled("authorization prompt was withdrawn"))
        })
    }
}

fn attempt_is_running(state: &BrokerState, attempt_id: &str, surface_id: &str) -> bool {
    state.attempts.get(attempt_id).is_some_and(|attempt| {
        attempt.surface_id == surface_id
            && attempt.public.status == AuthorizationAttemptStatus::Running
    })
}

fn prune_attempts(state: &mut BrokerState) {
    while state.attempts.len() >= MAX_RETAINED_ATTEMPTS {
        let oldest = state
            .attempts
            .iter()
            .filter(|(_, attempt)| attempt.public.status != AuthorizationAttemptStatus::Running)
            .min_by_key(|(_, attempt)| attempt.public.started_at_ms)
            .map(|(id, _)| id.clone());
        let Some(oldest) = oldest else {
            break;
        };
        state.attempts.remove(&oldest);
        state
            .notices
            .retain(|_, notice| notice.public.attempt_id != oldest);
    }
}

fn validate_surface_id(surface_id: &str) -> Result<(), HarnessError> {
    if surface_id.is_empty()
        || surface_id.len() > 128
        || !surface_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(HarnessError::invalid(
            "authorization surface_id must contain 1 to 128 safe ASCII characters",
        ));
    }
    Ok(())
}

fn now_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}
