#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::Value;
use ternilo_protocol::{
    AuthorizationCredentialKey, AuthorizationCredentialSpace, AuthorizationEntry,
    AuthorizationMethod, AuthorizationNotice, AuthorizationPrompt, HarnessError,
};
use tokio::sync::{Mutex, RwLock, watch};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizationCredentialState {
    pub configured: bool,
    pub writable: bool,
}

pub trait AuthorizationCredentialStore: Send + Sync + 'static {
    fn describe<'a>(
        &'a self,
        key: &'a AuthorizationCredentialKey,
    ) -> Pin<Box<dyn Future<Output = Result<AuthorizationCredentialState, HarnessError>> + Send + 'a>>;

    fn set_reference<'a>(
        &'a self,
        key: &'a str,
        value: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;

    fn set_record<'a>(
        &'a self,
        key: &'a str,
        kind: String,
        payload: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}

pub trait AuthorizationInteraction: Send + Sync + 'static {
    fn notify(&self, notice: AuthorizationNotice);

    fn prompt<'a>(
        &'a self,
        prompt: AuthorizationPrompt,
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>>;
}

pub trait AuthorizationFlow: Send + Sync + 'static {
    fn key(&self) -> AuthorizationCredentialKey;
    fn label(&self) -> &str;
    fn methods(&self) -> Vec<AuthorizationMethod>;

    fn run<'a>(
        &'a self,
        session: AuthorizationSession,
        writer: AuthorizationWriter,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationOutcome {
    Authorized,
    Cancelled,
}

#[derive(Clone)]
pub struct AuthorizationCancellation {
    receiver: watch::Receiver<bool>,
}

impl AuthorizationCancellation {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();
        if *receiver.borrow() {
            return;
        }
        while receiver.changed().await.is_ok() {
            if *receiver.borrow() {
                return;
            }
        }
    }
}

#[derive(Clone)]
pub struct AuthorizationSession {
    method: String,
    interaction: Arc<dyn AuthorizationInteraction>,
    cancellation: AuthorizationCancellation,
}

impl AuthorizationSession {
    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    #[must_use]
    pub fn cancellation(&self) -> AuthorizationCancellation {
        self.cancellation.clone()
    }

    pub fn notify(&self, notice: AuthorizationNotice) {
        self.interaction.notify(notice);
    }

    pub async fn prompt(&self, prompt: AuthorizationPrompt) -> Result<String, HarnessError> {
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => {
                Err(HarnessError::cancelled("authorization attempt was cancelled"))
            }
            answer = self.interaction.prompt(prompt) => answer,
        }
    }
}

#[derive(Clone)]
pub struct AuthorizationWriter {
    key: AuthorizationCredentialKey,
    store: Arc<dyn AuthorizationCredentialStore>,
    committed: Arc<AtomicBool>,
}

impl AuthorizationWriter {
    pub async fn set_secret(&self, value: String) -> Result<(), HarnessError> {
        if self.key.space != AuthorizationCredentialSpace::Reference {
            return Err(HarnessError::policy(
                "this authorization flow owns a credential record, not a secret reference",
            ));
        }
        self.store.set_reference(&self.key.key, value).await?;
        self.committed.store(true, Ordering::Release);
        Ok(())
    }

    pub async fn set_record(&self, kind: String, payload: Value) -> Result<(), HarnessError> {
        if self.key.space != AuthorizationCredentialSpace::Record {
            return Err(HarnessError::policy(
                "this authorization flow owns a secret reference, not a credential record",
            ));
        }
        self.store.set_record(&self.key.key, kind, payload).await?;
        self.committed.store(true, Ordering::Release);
        Ok(())
    }
}

struct RegisteredFlow {
    flow: Arc<dyn AuthorizationFlow>,
    label: String,
    methods: Vec<AuthorizationMethod>,
}

pub struct AuthorizationService {
    credentials: Arc<dyn AuthorizationCredentialStore>,
    flows: RwLock<BTreeMap<AuthorizationCredentialKey, RegisteredFlow>>,
    running: Mutex<BTreeMap<AuthorizationCredentialKey, watch::Sender<bool>>>,
}

impl AuthorizationService {
    #[must_use]
    pub fn new(credentials: Arc<dyn AuthorizationCredentialStore>) -> Arc<Self> {
        Arc::new(Self {
            credentials,
            flows: RwLock::new(BTreeMap::new()),
            running: Mutex::new(BTreeMap::new()),
        })
    }

    pub async fn register(&self, flow: Arc<dyn AuthorizationFlow>) -> Result<(), HarnessError> {
        let key = flow.key();
        key.validate()?;
        let label = flow.label().trim().to_owned();
        if label.is_empty() || label.chars().count() > 160 {
            return Err(HarnessError::invalid(
                "authorization flow label must contain 1 to 160 characters",
            ));
        }
        let methods = flow.methods();
        validate_methods(&methods)?;
        let mut flows = self.flows.write().await;
        if flows.contains_key(&key) {
            return Err(HarnessError::composition(format!(
                "an authorization flow is already registered for {:?}",
                key.key
            )));
        }
        flows.insert(
            key,
            RegisteredFlow {
                flow,
                label,
                methods,
            },
        );
        Ok(())
    }

    pub async fn unregister(&self, key: &AuthorizationCredentialKey) {
        self.flows.write().await.remove(key);
        self.cancel(key).await;
    }

    pub async fn list(&self) -> Result<Vec<AuthorizationEntry>, HarnessError> {
        let flows = self
            .flows
            .read()
            .await
            .iter()
            .map(|(key, registered)| {
                (
                    key.clone(),
                    registered.label.clone(),
                    registered.methods.clone(),
                )
            })
            .collect::<Vec<_>>();
        let running = self
            .running
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut entries = Vec::with_capacity(flows.len());
        for (key, label, methods) in flows {
            let state = self.credentials.describe(&key).await?;
            entries.push(AuthorizationEntry {
                in_flight: running.contains(&key),
                key,
                label,
                methods,
                configured: state.configured,
                writable: state.writable,
            });
        }
        Ok(entries)
    }

    pub async fn describe(
        &self,
        key: &AuthorizationCredentialKey,
    ) -> Result<Option<AuthorizationEntry>, HarnessError> {
        key.validate()?;
        let descriptor = self
            .flows
            .read()
            .await
            .get(key)
            .map(|registered| (registered.label.clone(), registered.methods.clone()));
        let Some((label, methods)) = descriptor else {
            return Ok(None);
        };
        let state = self.credentials.describe(key).await?;
        Ok(Some(AuthorizationEntry {
            key: key.clone(),
            label,
            methods,
            in_flight: self.running.lock().await.contains_key(key),
            configured: state.configured,
            writable: state.writable,
        }))
    }

    pub async fn begin(
        &self,
        key: AuthorizationCredentialKey,
        method: Option<String>,
        interaction: Arc<dyn AuthorizationInteraction>,
    ) -> Result<AuthorizationOutcome, HarnessError> {
        key.validate()?;
        let (flow, method) = {
            let flows = self.flows.read().await;
            let registered = flows.get(&key).ok_or_else(|| {
                HarnessError::invalid(format!(
                    "no authorization flow is registered for {:?}",
                    key.key
                ))
            })?;
            let method = method.unwrap_or_else(|| registered.methods[0].id.clone());
            if !registered
                .methods
                .iter()
                .any(|candidate| candidate.id == method)
            {
                return Err(HarnessError::invalid(format!(
                    "authorization flow for {:?} offers no method {method:?}",
                    key.key
                )));
            }
            (Arc::clone(&registered.flow), method)
        };
        let state = self.credentials.describe(&key).await?;
        if !state.writable {
            return Err(HarnessError::policy(format!(
                "credential {:?} is read-only and cannot be authorized interactively",
                key.key
            )));
        }
        let (cancellation, receiver) = watch::channel(false);
        {
            let mut running = self.running.lock().await;
            if running.contains_key(&key) {
                return Err(HarnessError::policy(format!(
                    "an authorization attempt for {:?} is already running",
                    key.key
                )));
            }
            running.insert(key.clone(), cancellation);
        }

        let committed = Arc::new(AtomicBool::new(false));
        let session = AuthorizationSession {
            method,
            interaction,
            cancellation: AuthorizationCancellation { receiver },
        };
        let writer = AuthorizationWriter {
            key: key.clone(),
            store: Arc::clone(&self.credentials),
            committed: Arc::clone(&committed),
        };
        let attempt_cancellation = session.cancellation();
        let result = tokio::select! {
            biased;
            () = attempt_cancellation.cancelled() => Ok(AuthorizationOutcome::Cancelled),
            result = flow.run(session, writer) => match result {
                Err(error) if error.is_cancelled() => Ok(AuthorizationOutcome::Cancelled),
                Err(error) => Err(error),
                Ok(()) => {
                    if !committed.load(Ordering::Acquire) {
                        Err(HarnessError::execution(format!(
                            "authorization flow for {:?} resolved without committing its credential",
                            key.key
                        )))
                    } else if !self.credentials.describe(&key).await?.configured {
                        Err(HarnessError::execution(format!(
                            "authorization flow for {:?} removed its credential before settling",
                            key.key
                        )))
                    } else {
                        Ok(AuthorizationOutcome::Authorized)
                    }
                }
            },
        };
        self.running.lock().await.remove(&key);
        result
    }

    pub async fn cancel(&self, key: &AuthorizationCredentialKey) {
        if let Some(cancellation) = self.running.lock().await.get(key) {
            let _ = cancellation.send(true);
        }
    }

    pub async fn cancel_all(&self) {
        for cancellation in self.running.lock().await.values() {
            let _ = cancellation.send(true);
        }
    }
}

fn validate_methods(methods: &[AuthorizationMethod]) -> Result<(), HarnessError> {
    if methods.is_empty() || methods.len() > 16 {
        return Err(HarnessError::invalid(
            "authorization flow requires 1 to 16 methods",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for method in methods {
        if method.id.trim().is_empty()
            || method.id.len() > 64
            || method.label.trim().is_empty()
            || method.label.chars().count() > 120
            || !ids.insert(method.id.as_str())
        {
            return Err(HarnessError::invalid(
                "authorization methods require unique non-empty ids and labels",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MemoryCredentials {
        references: StdMutex<BTreeMap<String, String>>,
        records: StdMutex<BTreeMap<String, (String, Value)>>,
    }

    impl AuthorizationCredentialStore for MemoryCredentials {
        fn describe<'a>(
            &'a self,
            key: &'a AuthorizationCredentialKey,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<AuthorizationCredentialState, HarnessError>> + Send + 'a,
            >,
        > {
            Box::pin(async move {
                let configured = match key.space {
                    AuthorizationCredentialSpace::Reference => self
                        .references
                        .lock()
                        .expect("reference store poisoned")
                        .contains_key(&key.key),
                    AuthorizationCredentialSpace::Record => self
                        .records
                        .lock()
                        .expect("record store poisoned")
                        .contains_key(&key.key),
                };
                Ok(AuthorizationCredentialState {
                    configured,
                    writable: true,
                })
            })
        }

        fn set_reference<'a>(
            &'a self,
            key: &'a str,
            value: String,
        ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                self.references
                    .lock()
                    .expect("reference store poisoned")
                    .insert(key.to_owned(), value);
                Ok(())
            })
        }

        fn set_record<'a>(
            &'a self,
            key: &'a str,
            kind: String,
            payload: Value,
        ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                self.records
                    .lock()
                    .expect("record store poisoned")
                    .insert(key.to_owned(), (kind, payload));
                Ok(())
            })
        }
    }

    struct ApiKeyFlow {
        key: AuthorizationCredentialKey,
        commit: bool,
    }

    impl AuthorizationFlow for ApiKeyFlow {
        fn key(&self) -> AuthorizationCredentialKey {
            self.key.clone()
        }

        fn label(&self) -> &'static str {
            "Fixture API key"
        }

        fn methods(&self) -> Vec<AuthorizationMethod> {
            vec![AuthorizationMethod {
                id: "api-key".to_owned(),
                label: "Enter API key".to_owned(),
            }]
        }

        fn run<'a>(
            &'a self,
            session: AuthorizationSession,
            writer: AuthorizationWriter,
        ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                session.notify(AuthorizationNotice {
                    message: "ready".to_owned(),
                    url: None,
                    code: None,
                });
                let secret = session
                    .prompt(AuthorizationPrompt::Secret {
                        message: "secret".to_owned(),
                        placeholder: None,
                    })
                    .await?;
                if self.commit {
                    writer.set_secret(secret).await?;
                }
                Ok(())
            })
        }
    }

    struct FixedInteraction {
        answer: StdMutex<Option<String>>,
        notices: StdMutex<Vec<AuthorizationNotice>>,
    }

    impl AuthorizationInteraction for FixedInteraction {
        fn notify(&self, notice: AuthorizationNotice) {
            self.notices
                .lock()
                .expect("notice list poisoned")
                .push(notice);
        }

        fn prompt<'a>(
            &'a self,
            _: AuthorizationPrompt,
        ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                self.answer
                    .lock()
                    .expect("answer slot poisoned")
                    .take()
                    .ok_or_else(|| HarnessError::execution("fixture has no answer"))
            })
        }
    }

    struct BlockingInteraction;

    impl AuthorizationInteraction for BlockingInteraction {
        fn notify(&self, _: AuthorizationNotice) {}

        fn prompt<'a>(
            &'a self,
            _: AuthorizationPrompt,
        ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>> {
            Box::pin(std::future::pending())
        }
    }

    fn key() -> AuthorizationCredentialKey {
        AuthorizationCredentialKey {
            space: AuthorizationCredentialSpace::Reference,
            key: "FIXTURE_KEY".to_owned(),
        }
    }

    #[tokio::test]
    async fn flow_commits_only_its_declared_credential_and_updates_catalog_state() {
        let credentials = Arc::new(MemoryCredentials::default());
        let credential_store: Arc<dyn AuthorizationCredentialStore> = credentials.clone();
        let service = AuthorizationService::new(credential_store);
        service
            .register(Arc::new(ApiKeyFlow {
                key: key(),
                commit: true,
            }))
            .await
            .unwrap();
        assert!(!service.list().await.unwrap()[0].configured);
        assert!(
            service
                .register(Arc::new(ApiKeyFlow {
                    key: key(),
                    commit: true,
                }))
                .await
                .is_err()
        );
        let interaction = Arc::new(FixedInteraction {
            answer: StdMutex::new(Some("fixture-secret".to_owned())),
            notices: StdMutex::new(Vec::new()),
        });
        let surface: Arc<dyn AuthorizationInteraction> = interaction.clone();
        assert_eq!(
            service.begin(key(), None, surface).await.unwrap(),
            AuthorizationOutcome::Authorized
        );
        assert_eq!(
            credentials
                .references
                .lock()
                .unwrap()
                .get("FIXTURE_KEY")
                .map(String::as_str),
            Some("fixture-secret")
        );
        assert!(service.list().await.unwrap()[0].configured);
        assert_eq!(interaction.notices.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn one_attempt_per_key_is_enforced_and_cancel_releases_the_slot() {
        let credentials: Arc<dyn AuthorizationCredentialStore> =
            Arc::new(MemoryCredentials::default());
        let service = AuthorizationService::new(credentials);
        service
            .register(Arc::new(ApiKeyFlow {
                key: key(),
                commit: true,
            }))
            .await
            .unwrap();
        let running = {
            let service = Arc::clone(&service);
            tokio::spawn(async move {
                service
                    .begin(key(), None, Arc::new(BlockingInteraction))
                    .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !service.list().await.unwrap()[0].in_flight {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            service
                .begin(key(), None, Arc::new(BlockingInteraction))
                .await
                .is_err()
        );
        service.cancel(&key()).await;
        assert_eq!(
            running.await.unwrap().unwrap(),
            AuthorizationOutcome::Cancelled
        );
        assert!(!service.list().await.unwrap()[0].in_flight);
    }

    #[tokio::test]
    async fn resolving_without_a_fresh_commit_is_rejected() {
        let credentials: Arc<dyn AuthorizationCredentialStore> =
            Arc::new(MemoryCredentials::default());
        let service = AuthorizationService::new(credentials);
        service
            .register(Arc::new(ApiKeyFlow {
                key: key(),
                commit: false,
            }))
            .await
            .unwrap();
        let error = service
            .begin(
                key(),
                None,
                Arc::new(FixedInteraction {
                    answer: StdMutex::new(Some("unused".to_owned())),
                    notices: StdMutex::new(Vec::new()),
                }),
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("without committing"), "{error}");
    }
}
