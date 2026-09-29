use std::{path::Path, time::Duration};

use ternilo_protocol::{DefaultModelSelection, HarnessError, SidebarOrdering};
use tokio_rusqlite::{Connection, rusqlite::OptionalExtension as _};

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS local_preferences (
    preference_key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL
);
";

pub(crate) struct LocalPreferences {
    connection: Connection,
}

impl LocalPreferences {
    pub(crate) async fn close(&self) -> Result<(), HarnessError> {
        self.connection.clone().close().await.map_err(|error| {
            HarnessError::execution(format!("close local preferences database: {error}"))
        })
    }

    pub(crate) async fn open(path: &Path) -> Result<Self, HarnessError> {
        let connection = Connection::open(path).await.map_err(|error| {
            HarnessError::execution(format!(
                "open local preferences database {}: {error}",
                path.display()
            ))
        })?;
        set_private_permissions(path).await?;
        connection
            .call(|database| -> Result<(), HarnessError> {
                database
                    .busy_timeout(Duration::from_secs(5))
                    .map_err(sqlite_error)?;
                database
                    .pragma_update(None, "journal_mode", "WAL")
                    .map_err(sqlite_error)?;
                database
                    .pragma_update(None, "synchronous", "FULL")
                    .map_err(sqlite_error)?;
                database.execute_batch(SCHEMA).map_err(sqlite_error)
            })
            .await
            .map_err(worker_error)?;
        Ok(Self { connection })
    }

    pub(crate) async fn default_model(&self) -> Result<DefaultModelSelection, HarnessError> {
        self.connection
            .call(|database| {
                let json = database
                    .query_row(
                        "SELECT value_json FROM local_preferences WHERE preference_key = 'default_model'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                let selection = json.map_or_else(
                    || Ok(DefaultModelSelection::default()),
                    |json| {
                        serde_json::from_str::<DefaultModelSelection>(&json).map_err(|error| {
                            HarnessError::execution(format!(
                                "parse local default model preference: {error}"
                            ))
                        })
                    },
                )?;
                selection.validate()?;
                Ok(selection)
            })
            .await
            .map_err(worker_error)
    }

    pub(crate) async fn set_default_model(
        &self,
        selection: DefaultModelSelection,
    ) -> Result<DefaultModelSelection, HarnessError> {
        selection.validate()?;
        let json = serde_json::to_string(&selection).map_err(|error| {
            HarnessError::execution(format!("serialize local default model preference: {error}"))
        })?;
        self.connection
            .call(move |database| {
                database
                    .execute(
                        "INSERT INTO local_preferences (preference_key, value_json)
                         VALUES ('default_model', ?1)
                         ON CONFLICT(preference_key) DO UPDATE SET value_json = excluded.value_json",
                        [json],
                    )
                    .map_err(sqlite_error)?;
                Ok(selection)
            })
            .await
            .map_err(worker_error)
    }

    pub(crate) async fn sidebar_ordering(&self) -> Result<SidebarOrdering, HarnessError> {
        self.connection
            .call(|database| {
                let json = database
                    .query_row(
                        "SELECT value_json FROM local_preferences WHERE preference_key = 'sidebar_ordering'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                let ordering = json.map_or_else(
                    || Ok(SidebarOrdering::default()),
                    |json| {
                        serde_json::from_str::<SidebarOrdering>(&json).map_err(|error| {
                            HarnessError::execution(format!(
                                "parse local sidebar ordering preference: {error}"
                            ))
                        })
                    },
                )?;
                ordering.validate()?;
                Ok(ordering)
            })
            .await
            .map_err(worker_error)
    }

    pub(crate) async fn set_sidebar_ordering(
        &self,
        ordering: SidebarOrdering,
    ) -> Result<SidebarOrdering, HarnessError> {
        ordering.validate()?;
        let json = serde_json::to_string(&ordering).map_err(|error| {
            HarnessError::execution(format!("serialize local sidebar ordering: {error}"))
        })?;
        self.connection
            .call(move |database| {
                database
                    .execute(
                        "INSERT INTO local_preferences (preference_key, value_json)
                         VALUES ('sidebar_ordering', ?1)
                         ON CONFLICT(preference_key) DO UPDATE SET value_json = excluded.value_json",
                        [json],
                    )
                    .map_err(sqlite_error)?;
                Ok(ordering)
            })
            .await
            .map_err(worker_error)
    }
}

#[allow(clippy::needless_pass_by_value)]
fn sqlite_error(error: tokio_rusqlite::rusqlite::Error) -> HarnessError {
    HarnessError::execution(format!("local preferences SQLite error: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn worker_error(error: tokio_rusqlite::Error<HarnessError>) -> HarnessError {
    HarnessError::execution(format!("local preferences worker error: {error}"))
}

#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|error| {
            HarnessError::execution(format!(
                "set private local preferences permissions {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
async fn set_private_permissions(_: &Path) -> Result<(), HarnessError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_model_is_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.sqlite3");
        let preferences = LocalPreferences::open(&path).await.unwrap();
        assert_eq!(
            preferences.default_model().await.unwrap(),
            DefaultModelSelection::ProfileDefault
        );
        let selected = DefaultModelSelection::NamedProvider {
            provider_id: "provider-a".to_owned(),
            model: "model-a".to_owned(),
            reasoning_effort: None,
        };
        preferences
            .set_default_model(selected.clone())
            .await
            .unwrap();
        drop(preferences);

        let reopened = LocalPreferences::open(&path).await.unwrap();
        assert_eq!(reopened.default_model().await.unwrap(), selected);
    }

    #[tokio::test]
    async fn sidebar_ordering_is_durable_on_the_host() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.sqlite3");
        let preferences = LocalPreferences::open(&path).await.unwrap();
        assert_eq!(
            preferences.sidebar_ordering().await.unwrap(),
            SidebarOrdering::default()
        );
        let ordering = SidebarOrdering {
            workspace_order: vec!["workspace-b".to_owned(), "workspace-a".to_owned()],
            session_order_by_account: std::collections::BTreeMap::from([
                (
                    "workspace-a".to_owned(),
                    vec!["session-2".to_owned(), "session-1".to_owned()],
                ),
                (
                    "__flat_sessions__".to_owned(),
                    vec!["session-1".to_owned(), "session-2".to_owned()],
                ),
            ]),
        };
        preferences
            .set_sidebar_ordering(ordering.clone())
            .await
            .unwrap();
        drop(preferences);

        let reopened = LocalPreferences::open(&path).await.unwrap();
        assert_eq!(reopened.sidebar_ordering().await.unwrap(), ordering);
    }
}
