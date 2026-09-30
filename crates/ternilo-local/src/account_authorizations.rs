use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use ternilo_protocol::{HarnessError, InputAuthor, InputProvenance};
use ternilo_transport::{NodeAccountAuthorization, NodeCleanupSnapshot, NodeInputAuthorization};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalServerBinding {
    pub server_url: String,
    pub node_id: String,
}

#[derive(Clone, Default)]
pub struct LocalApplicationOpenOptions {
    pub server_binding: Option<LocalServerBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptedInput {
    user_id: String,
    authorization: NodeInputAuthorization,
}

#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    storage_instance_id: String,
    binding: Option<LocalServerBinding>,
    server_id: Option<String>,
    tenant_id: Option<String>,
    credential_id: Option<String>,
    inputs: BTreeMap<String, AcceptedInput>,
    revoked_through: BTreeMap<String, u64>,
    cleanup_receipts: BTreeMap<String, ternilo_transport::NodeCleanupReceipt>,
}

struct State {
    durable: Document,
    authorities: BTreeMap<String, NodeAccountAuthorization>,
    synchronized: bool,
}

pub(crate) struct AccountAuthorizations {
    path: PathBuf,
    state: Mutex<State>,
    admission: tokio::sync::RwLock<()>,
}

impl AccountAuthorizations {
    pub(crate) async fn open(
        data_dir: &Path,
        binding: Option<LocalServerBinding>,
    ) -> Result<Self, HarnessError> {
        let path = data_dir.join("node-authorizations.json");
        let mut document: Document = match tokio::fs::read(&path).await {
            Ok(data) => serde_json::from_slice(&data).map_err(|error| {
                HarnessError::execution(format!("read Node authorizations: {error}"))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Document {
                storage_instance_id: format!("ter_ns_{:032x}", rand::random::<u128>()),
                ..Document::default()
            },
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "read Node authorizations: {error}"
                )));
            }
        };
        if let Some(binding) = binding {
            if document
                .binding
                .as_ref()
                .is_some_and(|stored| stored != &binding)
            {
                return Err(HarnessError::conflict(
                    "this data directory is already bound to another Server or Node; use a separate data directory",
                ));
            }
            document.binding = Some(binding);
        }
        save(&path, &document).await?;
        Ok(Self {
            path,
            admission: tokio::sync::RwLock::new(()),
            state: Mutex::new(State {
                durable: document,
                authorities: BTreeMap::new(),
                synchronized: false,
            }),
        })
    }

    pub(crate) async fn binding(&self) -> Option<LocalServerBinding> {
        self.state.lock().await.durable.binding.clone()
    }
    pub(crate) async fn storage_instance_id(&self) -> String {
        self.state.lock().await.durable.storage_instance_id.clone()
    }

    pub(crate) async fn input_admission(&self) -> tokio::sync::RwLockReadGuard<'_, ()> {
        self.admission.read().await
    }

    pub(crate) async fn synchronize(
        &self,
        snapshot: &NodeCleanupSnapshot,
    ) -> Result<(), HarnessError> {
        snapshot.validate()?;
        // An accepted input must finish entering the durable queue before a
        // new authorization snapshot can prune that queue and acknowledge cleanup.
        let _admission = self.admission.write().await;
        let mut state = self.state.lock().await;
        let mut document = state.durable.clone();
        let binding = document.binding.as_ref().ok_or_else(|| {
            HarnessError::policy("Node cleanup requires a persistent Server binding")
        })?;
        if binding.node_id != snapshot.executor_id.as_str()
            || document
                .server_id
                .as_ref()
                .is_some_and(|server| server != &snapshot.server_id)
            || document
                .tenant_id
                .as_deref()
                .is_some_and(|tenant| tenant != snapshot.tenant_id.as_str())
        {
            return Err(HarnessError::policy(
                "Node cleanup belongs to another Server or Node",
            ));
        }
        document.server_id = Some(snapshot.server_id.clone());
        document.tenant_id = Some(snapshot.tenant_id.to_string());
        document.credential_id = Some(snapshot.credential_id.clone());
        for request in &snapshot.requests {
            let revision = document
                .revoked_through
                .entry(request.user_id.to_string())
                .or_default();
            *revision = (*revision).max(request.status_revision);
        }
        let mut authorities = BTreeMap::new();
        for account in &snapshot.authorizations {
            if authorities
                .insert(account.user_id.to_string(), account.clone())
                .is_some()
            {
                return Err(HarnessError::invalid(
                    "duplicate Node account authorization",
                ));
            }
            if !account.active {
                let revision = document
                    .revoked_through
                    .entry(account.user_id.to_string())
                    .or_default();
                *revision = (*revision).max(account.status_revision);
            }
        }
        if document != state.durable {
            save(&self.path, &document).await?;
            state.durable = document;
        }
        state.authorities = authorities;
        state.synchronized = true;
        Ok(())
    }

    pub(crate) async fn disconnect(&self) {
        self.state.lock().await.synchronized = false;
    }

    pub(crate) async fn receipt(&self, id: &str) -> Option<ternilo_transport::NodeCleanupReceipt> {
        self.state
            .lock()
            .await
            .durable
            .cleanup_receipts
            .get(id)
            .cloned()
    }
    pub(crate) async fn save_receipt(
        &self,
        receipt: ternilo_transport::NodeCleanupReceipt,
    ) -> Result<(), HarnessError> {
        let mut state = self.state.lock().await;
        if state
            .durable
            .cleanup_receipts
            .get(&receipt.request_id)
            .is_some_and(|previous| {
                previous == &receipt
                    || previous.state == ternilo_transport::NodeCleanupState::Confirmed
            })
        {
            return Ok(());
        }
        let mut document = state.durable.clone();
        document
            .cleanup_receipts
            .insert(receipt.request_id.clone(), receipt);
        save(&self.path, &document).await?;
        state.durable = document;
        Ok(())
    }

    pub(crate) async fn accept(
        &self,
        provenance: &InputProvenance,
        authorization: &NodeInputAuthorization,
    ) -> Result<(), HarnessError> {
        provenance.validate()?;
        let InputAuthor::Account { user_id, .. } = &provenance.author else {
            return Err(HarnessError::policy(
                "Node input authorization requires an account author",
            ));
        };
        let mut state = self.state.lock().await;
        if state.durable.binding.is_none() || !state.synchronized {
            return Err(HarnessError::unavailable(
                "Node account authorization has not synchronized with the Server",
            ));
        }
        if state.durable.credential_id.as_ref() != Some(&authorization.credential_id) {
            return Err(HarnessError::policy(
                "input authorization belongs to another Node credential instance",
            ));
        }
        if state
            .durable
            .revoked_through
            .get(user_id.as_str())
            .is_some_and(|revoked| authorization.status_revision <= *revoked)
        {
            return Err(HarnessError::policy(
                "input account authorization was revoked",
            ));
        }
        if state
            .authorities
            .get(user_id.as_str())
            .is_some_and(|account| {
                !account.active || account.status_revision > authorization.status_revision
            })
        {
            return Err(HarnessError::policy(
                "input account authorization is no longer current",
            ));
        }
        let input = AcceptedInput {
            user_id: user_id.to_string(),
            authorization: authorization.clone(),
        };
        if state
            .durable
            .inputs
            .get(provenance.input_id.as_str())
            .is_some_and(|old| old != &input)
        {
            return Err(HarnessError::conflict(
                "accepted account input authorization cannot change",
            ));
        }
        let mut document = state.durable.clone();
        document
            .inputs
            .insert(provenance.input_id.to_string(), input);
        save(&self.path, &document).await?;
        state.durable = document;
        state.authorities.insert(
            user_id.to_string(),
            NodeAccountAuthorization {
                user_id: user_id.clone(),
                status_revision: authorization.status_revision,
                active: true,
            },
        );
        Ok(())
    }

    pub(crate) async fn check(
        &self,
        provenance: Option<&InputProvenance>,
    ) -> Result<(), HarnessError> {
        let state = self.state.lock().await;
        if state.durable.binding.is_none() {
            return Ok(());
        }
        let Some(provenance) = provenance else {
            return Err(HarnessError::policy(
                "bound Node execution requires input provenance",
            ));
        };
        match &provenance.author {
            InputAuthor::Local => Ok(()),
            InputAuthor::Automation { .. } => Err(HarnessError::policy(
                "automated Node execution requires its original input authorization",
            )),
            InputAuthor::Account { user_id, .. } => {
                let input = state
                    .durable
                    .inputs
                    .get(provenance.input_id.as_str())
                    .filter(|input| input.user_id == user_id.as_str())
                    .ok_or_else(|| {
                        HarnessError::policy(
                            "account input has no trusted Node acceptance evidence",
                        )
                    })?;
                if state
                    .durable
                    .revoked_through
                    .get(user_id.as_str())
                    .is_some_and(|revision| input.authorization.status_revision <= *revision)
                    || state.durable.credential_id.as_ref()
                        != Some(&input.authorization.credential_id)
                {
                    return Err(HarnessError::policy(
                        "input account authorization was revoked",
                    ));
                }
                if !state.synchronized {
                    return Err(HarnessError::unavailable(
                        "Node account authorization awaits Server synchronization",
                    ));
                }
                let account = state.authorities.get(user_id.as_str()).ok_or_else(|| {
                    HarnessError::unavailable(
                        "Node account authorization awaits Server synchronization",
                    )
                })?;
                if !account.active || account.status_revision != input.authorization.status_revision
                {
                    return Err(HarnessError::policy(
                        "input account authorization is no longer current",
                    ));
                }
                Ok(())
            }
        }
    }
}

async fn save(path: &Path, document: &Document) -> Result<(), HarnessError> {
    let data = serde_json::to_vec(document)
        .map_err(|error| HarnessError::execution(format!("encode Node authorizations: {error}")))?;
    crate::persistence::atomic_replace(path, &data, true).await
}
