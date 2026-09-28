//! Local model connections keep browser approval codes and credentials out of the Web API.

use crate::{LocalCredentials, persistence::atomic_replace};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use ternilo_protocol::{
    HarnessError, ModelDeviceAuthorization, ModelDevicePoll, ModelDeviceSession, ProviderProfile,
};
use tokio::sync::Mutex;

mod http;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelConnection {
    pub connection_id: String,
    pub name: String,
    pub disconnected: bool,
    pub server_url: String,
    pub session: ModelDeviceSession,
    pub known_providers: Vec<ProviderProfile>,
}

#[derive(Serialize)]
pub struct ConnectionAuthorization {
    pub attempt_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at_ms: u64,
    pub interval: u64,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ConnectionPoll {
    Pending { interval: u64 },
    Denied,
    Expired,
    Connected { connection: Box<ModelConnection> },
}

struct Pending {
    name: String,
    server_url: String,
    authorization: ModelDeviceAuthorization,
    expires_at: u64,
    next_poll_at: u64,
}

pub struct LocalModelConnections {
    path: PathBuf,
    entries: Mutex<BTreeMap<String, ModelConnection>>,
    pending: Mutex<BTreeMap<String, Pending>>,
    client: reqwest::Client,
}

impl LocalModelConnections {
    pub async fn open(root: PathBuf) -> Result<Arc<Self>, HarnessError> {
        let path = root.join("model-connections.json");
        let entries = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice::<BTreeMap<String, ModelConnection>>(&bytes)
                .map_err(|error| {
                    HarnessError::execution(format!("read model connections: {error}"))
                })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "read model connections: {error}"
                )));
            }
        };
        for (id, connection) in &entries {
            if id != &connection.connection_id {
                return Err(HarnessError::invalid(
                    "model connection identity does not match its key",
                ));
            }
            http::server_url(&connection.server_url)?;
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                HarnessError::execution(format!("build model connection client: {error}"))
            })?;
        Ok(Arc::new(Self {
            path,
            entries: Mutex::new(entries),
            pending: Mutex::new(BTreeMap::new()),
            client,
        }))
    }

    pub async fn list(&self) -> Vec<ModelConnection> {
        self.entries
            .lock()
            .await
            .values()
            .filter(|connection| !connection.disconnected)
            .cloned()
            .collect()
    }
    pub(crate) async fn stored(&self) -> Vec<ModelConnection> {
        self.entries.lock().await.values().cloned().collect()
    }

    pub async fn begin(
        &self,
        server_url: &str,
        name: &str,
    ) -> Result<ConnectionAuthorization, HarnessError> {
        let server_url = http::server_url(server_url)?;
        if name.trim().is_empty() || name.chars().count() > 120 {
            return Err(HarnessError::invalid(
                "connection name must contain 1 to 120 characters",
            ));
        }
        let authorization: ModelDeviceAuthorization = http::json(
            self.client
                .post(format!("{server_url}/api/v1/model-device/authorize"))
                .json(&serde_json::json!({"device_name":name.trim()})),
        )
        .await?;
        if !(1..=3600).contains(&authorization.expires_in)
            || !(1..=60).contains(&authorization.interval)
        {
            return Err(HarnessError::invalid(
                "server returned an invalid device authorization lifetime",
            ));
        }
        let id = format!("{:032x}", rand::random::<u128>());
        let now = now_ms()?;
        let expires_at = now + authorization.expires_in * 1000;
        let result = ConnectionAuthorization {
            attempt_id: id.clone(),
            user_code: authorization.user_code.clone(),
            verification_uri: format!(
                "{server_url}/model-connect?code={}",
                authorization.user_code
            ),
            expires_at_ms: expires_at,
            interval: authorization.interval,
        };
        let mut pending = self.pending.lock().await;
        pending.retain(|_, entry| entry.expires_at > now);
        if pending.len() >= 16 {
            return Err(HarnessError::conflict("too many model connection attempts"));
        }
        pending.insert(
            id,
            Pending {
                name: name.trim().to_owned(),
                server_url,
                next_poll_at: now + authorization.interval * 1000,
                authorization,
                expires_at,
            },
        );
        Ok(result)
    }

    pub async fn poll(
        &self,
        id: &str,
        credentials: &LocalCredentials,
    ) -> Result<ConnectionPoll, HarnessError> {
        let mut pending = self.pending.lock().await;
        let now = now_ms()?;
        let attempt = pending.get_mut(id).ok_or_else(|| {
            HarnessError::invalid("model connection attempt is no longer available")
        })?;
        if now >= attempt.expires_at {
            pending.remove(id);
            return Ok(ConnectionPoll::Expired);
        }
        if now < attempt.next_poll_at {
            return Ok(ConnectionPoll::Pending {
                interval: attempt.authorization.interval,
            });
        }
        attempt.next_poll_at = now + attempt.authorization.interval * 1000;
        let result: ModelDevicePoll = http::json(
            self.client
                .post(format!("{}/api/v1/model-device/token", attempt.server_url))
                .json(&serde_json::json!({"device_code":attempt.authorization.device_code})),
        )
        .await?;
        let result = match result {
            ModelDevicePoll::Pending { interval } | ModelDevicePoll::SlowDown { interval } => {
                attempt.authorization.interval = interval.clamp(1, 60);
                attempt.next_poll_at = now + attempt.authorization.interval * 1000;
                return Ok(ConnectionPoll::Pending {
                    interval: attempt.authorization.interval,
                });
            }
            ModelDevicePoll::Denied => ConnectionPoll::Denied,
            ModelDevicePoll::Expired => ConnectionPoll::Expired,
            ModelDevicePoll::Authorized { token, session } => {
                let session = self
                    .complete_catalog(&attempt.server_url, &token, *session)
                    .await?;
                let mut connection = ModelConnection {
                    connection_id: id.to_owned(),
                    disconnected: false,
                    name: attempt.name.clone(),
                    server_url: attempt.server_url.clone(),
                    session,
                    known_providers: Vec::new(),
                };
                connection.remember_providers();
                credentials.set(credential_reference(id), token).await?;
                self.save(connection.clone()).await?;
                ConnectionPoll::Connected {
                    connection: Box::new(connection),
                }
            }
        };
        pending.remove(id);
        Ok(result)
    }

    pub async fn cancel(&self, id: &str) {
        self.pending.lock().await.remove(id);
    }

    pub async fn refresh(
        &self,
        id: &str,
        credentials: &LocalCredentials,
    ) -> Result<ModelConnection, HarnessError> {
        let mut connection = self.get(id).await?;
        let token = connection_token(credentials, id).await?;
        let session: ModelDeviceSession = http::json(
            self.client
                .get(format!("{}/v1/model-device", connection.server_url))
                .bearer_auth(&token),
        )
        .await?;
        if session.identity.device_id != connection.session.identity.device_id
            || session.identity.user_id != connection.session.identity.user_id
        {
            return Err(HarnessError::policy(
                "server changed the identity of this model connection",
            ));
        }
        connection.session = self
            .complete_catalog(&connection.server_url, &token, session)
            .await?;
        connection.remember_providers();
        self.save(connection.clone()).await?;
        Ok(connection)
    }

    pub async fn remove(
        &self,
        id: &str,
        credentials: &LocalCredentials,
        revoke: bool,
    ) -> Result<(), HarnessError> {
        let connection = self.get(id).await?;
        if revoke {
            let token = connection_token(credentials, id).await?;
            http::empty(
                self.client
                    .delete(format!("{}/v1/model-device", connection.server_url))
                    .bearer_auth(token),
            )
            .await?;
        }
        let mut entries = self.entries.lock().await;
        let mut next = entries.clone();
        if let Some(connection) = next.get_mut(id) {
            connection.disconnected = true;
        }
        self.persist(&next).await?;
        *entries = next;
        if credentials
            .resolve_value(&credential_reference(id))
            .await?
            .is_some()
        {
            credentials.remove(&credential_reference(id)).await?;
        }
        Ok(())
    }

    async fn complete_catalog(
        &self,
        server: &str,
        token: &str,
        mut session: ModelDeviceSession,
    ) -> Result<ModelDeviceSession, HarnessError> {
        let mut seen = std::collections::BTreeSet::new();
        while let Some(cursor) = session.next_cursor.take() {
            if !seen.insert(cursor.clone()) {
                return Err(HarnessError::execution(
                    "Server repeated a model catalog cursor",
                ));
            }
            let page: ModelDeviceSession = http::json(
                self.client
                    .get(format!("{server}/v1/model-device"))
                    .query(&[("cursor", cursor)])
                    .bearer_auth(token),
            )
            .await?;
            if page.identity.device_id != session.identity.device_id
                || page.identity.user_id != session.identity.user_id
            {
                return Err(HarnessError::policy(
                    "Server changed the identity while reading the model catalog",
                ));
            }
            session.grants.extend(page.grants);
            session.providers.extend(page.providers);
            session.next_cursor = page.next_cursor;
        }
        Ok(session)
    }

    async fn get(&self, id: &str) -> Result<ModelConnection, HarnessError> {
        self.entries
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| HarnessError::invalid("model connection does not exist"))
    }
    async fn save(&self, connection: ModelConnection) -> Result<(), HarnessError> {
        let mut entries = self.entries.lock().await;
        let mut next = entries.clone();
        next.insert(connection.connection_id.clone(), connection);
        self.persist(&next).await?;
        *entries = next;
        Ok(())
    }
    async fn persist(
        &self,
        entries: &BTreeMap<String, ModelConnection>,
    ) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec_pretty(entries)
            .map_err(|error| HarnessError::execution(error.to_string()))?;
        atomic_replace(&self.path, &bytes, true).await
    }
}

mod sources;
pub use sources::is_connection_provider;

fn credential_reference(id: &str) -> String {
    format!("TERNILO_MODEL_CONNECTION_{}", id.to_ascii_uppercase())
}
async fn connection_token(
    credentials: &LocalCredentials,
    id: &str,
) -> Result<String, HarnessError> {
    credentials
        .resolve_value(&credential_reference(id))
        .await?
        .ok_or_else(|| {
            HarnessError::policy("model connection credential is missing; connect again")
        })
}
fn now_ms() -> Result<u64, HarnessError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| HarnessError::execution(error.to_string()))?
            .as_millis(),
    )
    .map_err(|_| HarnessError::execution("clock exceeds supported range"))
}
