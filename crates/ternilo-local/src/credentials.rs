use std::{collections::BTreeMap, path::PathBuf, pin::Pin, sync::Arc, time::UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ternilo_kernel::SecretResolver;
use ternilo_protocol::{
    CredentialInventory, CredentialRecordInfo, CredentialReferenceInfo, CredentialSource,
    HarnessError,
};
use tokio::sync::Mutex;

use crate::persistence::atomic_replace;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredentialRecord {
    kind: String,
    payload: Value,
    updated_at_ms: u64,
}

pub struct LocalCredentials {
    references_path: PathBuf,
    records_path: PathBuf,
    values: Mutex<BTreeMap<String, String>>,
    records: Mutex<BTreeMap<String, StoredCredentialRecord>>,
}

impl LocalCredentials {
    pub async fn open(root: PathBuf) -> Result<Arc<Self>, HarnessError> {
        tokio::fs::create_dir_all(&root).await.map_err(|error| {
            HarnessError::execution(format!(
                "create credential directory {}: {error}",
                root.display()
            ))
        })?;
        let references_path = root.join("secrets/credentials.json");
        let values: BTreeMap<String, String> = read_private_json(&references_path).await?;
        for (name, value) in &values {
            validate_name(name)?;
            validate_secret_value(value)?;
        }
        let records_path = root.join("secrets/credential-records.json");
        let records: BTreeMap<String, StoredCredentialRecord> =
            read_private_json(&records_path).await?;
        for (key, record) in &records {
            validate_record_key(key)?;
            validate_record(&record.kind, &record.payload)?;
        }
        Ok(Arc::new(Self {
            references_path,
            records_path,
            values: Mutex::new(values),
            records: Mutex::new(records),
        }))
    }

    pub async fn names(&self) -> Vec<String> {
        self.values.lock().await.keys().cloned().collect()
    }

    pub async fn resolve_value(&self, name: &str) -> Result<Option<String>, HarnessError> {
        validate_name(name)?;
        if let Some(value) = nonempty_environment(name) {
            return Ok(Some(value));
        }
        Ok(self.values.lock().await.get(name).cloned())
    }

    pub async fn inventory(&self) -> CredentialInventory {
        let values = self.values.lock().await;
        let references = values
            .keys()
            .map(|reference| {
                let environment = nonempty_environment(reference).is_some();
                CredentialReferenceInfo {
                    reference: reference.clone(),
                    configured: true,
                    source: Some(if environment {
                        CredentialSource::Environment
                    } else {
                        CredentialSource::Managed
                    }),
                    writable: !environment,
                }
            })
            .collect();
        drop(values);
        let records = self
            .records
            .lock()
            .await
            .iter()
            .map(|(key, record)| CredentialRecordInfo {
                key: key.clone(),
                kind: record.kind.clone(),
                updated_at_ms: record.updated_at_ms,
            })
            .collect();
        CredentialInventory {
            references,
            records,
        }
    }

    pub async fn describe(&self, name: &str) -> Result<CredentialReferenceInfo, HarnessError> {
        validate_name(name)?;
        if nonempty_environment(name).is_some() {
            return Ok(CredentialReferenceInfo {
                reference: name.to_owned(),
                configured: true,
                source: Some(CredentialSource::Environment),
                writable: false,
            });
        }
        let configured = self.values.lock().await.contains_key(name);
        Ok(CredentialReferenceInfo {
            reference: name.to_owned(),
            configured,
            source: configured.then_some(CredentialSource::Managed),
            writable: true,
        })
    }

    pub async fn set(&self, name: String, value: String) -> Result<(), HarnessError> {
        validate_name(&name)?;
        validate_secret_value(&value)?;
        if nonempty_environment(&name).is_some() {
            return Err(HarnessError::policy(format!(
                "credential reference {name:?} is shadowed by a read-only environment value"
            )));
        }
        let mut guard = self.values.lock().await;
        let mut next = guard.clone();
        next.insert(name, value);
        self.persist_references(&next).await?;
        *guard = next;
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> Result<(), HarnessError> {
        validate_name(name)?;
        if nonempty_environment(name).is_some() {
            return Err(HarnessError::policy(format!(
                "credential reference {name:?} is shadowed by a read-only environment value"
            )));
        }
        let mut guard = self.values.lock().await;
        let mut next = guard.clone();
        if next.remove(name).is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown credential {name:?}"
            )));
        }
        self.persist_references(&next).await?;
        *guard = next;
        Ok(())
    }

    pub async fn set_record(
        &self,
        key: String,
        kind: String,
        payload: Value,
    ) -> Result<CredentialRecordInfo, HarnessError> {
        validate_record_key(&key)?;
        validate_record(&kind, &payload)?;
        let record = StoredCredentialRecord {
            kind,
            payload,
            updated_at_ms: now_ms()?,
        };
        let mut guard = self.records.lock().await;
        let mut next = guard.clone();
        next.insert(key.clone(), record.clone());
        self.persist_records(&next).await?;
        *guard = next;
        Ok(CredentialRecordInfo {
            key,
            kind: record.kind,
            updated_at_ms: record.updated_at_ms,
        })
    }

    pub async fn record(&self, key: &str) -> Result<Option<(String, Value)>, HarnessError> {
        validate_record_key(key)?;
        Ok(self
            .records
            .lock()
            .await
            .get(key)
            .map(|record| (record.kind.clone(), record.payload.clone())))
    }

    pub async fn delete_record(&self, key: &str) -> Result<(), HarnessError> {
        validate_record_key(key)?;
        let mut guard = self.records.lock().await;
        let mut next = guard.clone();
        if next.remove(key).is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown credential record {key:?}"
            )));
        }
        self.persist_records(&next).await?;
        *guard = next;
        Ok(())
    }

    async fn persist_references(
        &self,
        values: &BTreeMap<String, String>,
    ) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec_pretty(values).map_err(|error| {
            HarnessError::execution(format!("serialize local credentials: {error}"))
        })?;
        atomic_replace(&self.references_path, &bytes, true).await
    }

    async fn persist_records(
        &self,
        records: &BTreeMap<String, StoredCredentialRecord>,
    ) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec_pretty(records).map_err(|error| {
            HarnessError::execution(format!("serialize local credential records: {error}"))
        })?;
        atomic_replace(&self.records_path, &bytes, true).await
    }
}

impl SecretResolver for LocalCredentials {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Option<String>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move { self.resolve_value(name).await })
    }
}

impl ternilo_authorization::AuthorizationCredentialStore for LocalCredentials {
    fn describe<'a>(
        &'a self,
        key: &'a ternilo_protocol::AuthorizationCredentialKey,
    ) -> Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        ternilo_authorization::AuthorizationCredentialState,
                        HarnessError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            match key.space {
                ternilo_protocol::AuthorizationCredentialSpace::Reference => {
                    let reference = self.describe(&key.key).await?;
                    Ok(ternilo_authorization::AuthorizationCredentialState {
                        configured: reference.configured,
                        writable: reference.writable,
                    })
                }
                ternilo_protocol::AuthorizationCredentialSpace::Record => {
                    Ok(ternilo_authorization::AuthorizationCredentialState {
                        configured: self.record(&key.key).await?.is_some(),
                        writable: true,
                    })
                }
            }
        })
    }

    fn set_reference<'a>(
        &'a self,
        key: &'a str,
        value: String,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.set(key.to_owned(), value).await })
    }

    fn set_record<'a>(
        &'a self,
        key: &'a str,
        kind: String,
        payload: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            LocalCredentials::set_record(self, key.to_owned(), kind, payload)
                .await
                .map(|_| ())
        })
    }
}

async fn read_private_json<T>(path: &PathBuf) -> Result<T, HarnessError>
where
    T: serde::de::DeserializeOwned + Default,
{
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| HarnessError::execution(format!("parse {}: {error}", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(HarnessError::execution(format!(
            "read {}: {error}",
            path.display()
        ))),
    }
}

fn nonempty_environment(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn validate_secret_value(value: &str) -> Result<(), HarnessError> {
    if value.is_empty() || value.len() > 64 * 1024 {
        Err(HarnessError::invalid(
            "credential value must contain 1 to 65536 bytes",
        ))
    } else {
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<(), HarnessError> {
    let mut bytes = name.bytes();
    let valid_first = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_');
    if !valid_first || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        return Err(HarnessError::invalid(
            "credential name must match [A-Za-z_][A-Za-z0-9_]*",
        ));
    }
    Ok(())
}

fn validate_record_key(key: &str) -> Result<(), HarnessError> {
    let Some((scope, id)) = key.split_once('/') else {
        return Err(HarnessError::invalid(
            "credential record key must be <plugin-scope>/<id>",
        ));
    };
    if key.len() > 200
        || scope.is_empty()
        || id.is_empty()
        || id.contains('/')
        || !scope
            .bytes()
            .chain(id.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(HarnessError::invalid(
            "credential record key segments must use ASCII letters, digits, dot, dash, or underscore",
        ));
    }
    Ok(())
}

fn validate_record(kind: &str, payload: &Value) -> Result<(), HarnessError> {
    if kind.is_empty()
        || kind.len() > 64
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(HarnessError::invalid(
            "credential record kind has an invalid format",
        ));
    }
    let size = serde_json::to_vec(payload)
        .map_err(|error| HarnessError::invalid(format!("serialize credential payload: {error}")))?
        .len();
    if size > 64 * 1024 {
        return Err(HarnessError::invalid(
            "credential record payload may not exceed 64 KiB",
        ));
    }
    Ok(())
}

fn now_ms() -> Result<u64, HarnessError> {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[tokio::test]
    async fn persists_secret_references_and_records_without_listing_values() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ternilo-credentials-test-{}-{nonce}",
            std::process::id()
        ));
        let credentials = LocalCredentials::open(root.clone()).await.unwrap();
        credentials
            .set("TEST_API_KEY".to_owned(), "secret-value".to_owned())
            .await
            .unwrap();
        credentials
            .set_record(
                "provider/demo".to_owned(),
                "grant".to_owned(),
                serde_json::json!({ "refresh_token": "record-secret" }),
            )
            .await
            .unwrap();
        assert_eq!(credentials.names().await, vec!["TEST_API_KEY"]);
        assert_eq!(
            credentials.resolve("TEST_API_KEY").await.unwrap(),
            Some("secret-value".to_owned())
        );
        let inventory = credentials.inventory().await;
        assert_eq!(inventory.references.len(), 1);
        assert_eq!(inventory.records.len(), 1);
        assert!(
            !serde_json::to_string(&inventory)
                .unwrap()
                .contains("secret")
        );
        drop(credentials);

        let reopened = LocalCredentials::open(root.clone()).await.unwrap();
        assert_eq!(
            reopened.resolve("TEST_API_KEY").await.unwrap(),
            Some("secret-value".to_owned())
        );
        assert_eq!(
            reopened.record("provider/demo").await.unwrap().unwrap().0,
            "grant"
        );
        #[cfg(unix)]
        for path in [
            "secrets/credentials.json",
            "secrets/credential-records.json",
        ] {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(root.join(path))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
