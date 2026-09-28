use std::collections::{BTreeMap, BTreeSet};

use ternilo_protocol::SessionId;

use super::{
    HarnessError, LocalInboxStore, OptionalExtension as _, database_error, params, rusqlite,
};
use crate::LocalSession;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS session_execution_scopes (
    session_id TEXT PRIMARY KEY,
    parent_session_id TEXT,
    is_subagent INTEGER NOT NULL CHECK (is_subagent IN (0, 1)),
    execution_scope TEXT NOT NULL,
    CHECK (parent_session_id IS NULL OR parent_session_id <> session_id)
);
";

#[derive(Clone)]
struct SessionBinding {
    session_id: String,
    parent_session_id: Option<String>,
    is_subagent: bool,
}

impl From<&LocalSession> for SessionBinding {
    fn from(session: &LocalSession) -> Self {
        Self {
            session_id: session.identity.session_id.as_str().to_owned(),
            parent_session_id: session
                .parent_session_id
                .as_ref()
                .map(|id| id.as_str().to_owned()),
            is_subagent: session.subagent.is_some(),
        }
    }
}

impl SessionBinding {
    fn validate(&self) -> Result<(), HarnessError> {
        SessionId::new(&self.session_id).validate()?;
        if let Some(parent) = &self.parent_session_id {
            SessionId::new(parent).validate()?;
            if parent == &self.session_id {
                return Err(HarnessError::invalid(
                    "execution scope session cannot be its own parent",
                ));
            }
        }
        Ok(())
    }
}

impl LocalInboxStore {
    /// Initialize only trusted session metadata; deleted parent bindings remain available.
    pub(crate) async fn initialize_execution_scopes(
        &self,
        sessions: &[LocalSession],
    ) -> Result<(), HarnessError> {
        let bindings = sessions
            .iter()
            .map(|session| {
                let binding = SessionBinding::from(session);
                (binding.session_id.clone(), binding)
            })
            .collect::<BTreeMap<_, _>>();
        let namespace = self.stream_id.clone();
        self.call(move |database| {
            let transaction = database.transaction().map_err(database_error)?;
            for binding in bindings.values() {
                resolve(
                    &transaction,
                    &namespace,
                    binding,
                    &bindings,
                    &mut BTreeSet::new(),
                )?;
            }
            transaction.commit().map_err(database_error)
        })
        .await
    }

    pub(crate) async fn register_execution_scope(
        &self,
        session: &LocalSession,
    ) -> Result<String, HarnessError> {
        let binding = SessionBinding::from(session);
        let namespace = self.stream_id.clone();
        self.call(move |database| {
            let transaction = database.transaction().map_err(database_error)?;
            let scope = resolve(
                &transaction,
                &namespace,
                &binding,
                &BTreeMap::new(),
                &mut BTreeSet::new(),
            )?;
            transaction.commit().map_err(database_error)?;
            Ok(scope)
        })
        .await
    }

    pub(crate) async fn execution_scope(&self, session_id: &str) -> Result<String, HarnessError> {
        SessionId::new(session_id).validate()?;
        let session_id = session_id.to_owned();
        self.call(move |database| {
            lookup_scope(database, &session_id)?.ok_or_else(|| {
                HarnessError::invalid(format!(
                    "unknown execution scope for session {session_id:?}"
                ))
            })
        })
        .await
    }
}

fn lookup_scope(
    database: &rusqlite::Connection,
    session_id: &str,
) -> Result<Option<String>, HarnessError> {
    database
        .query_row(
            "SELECT execution_scope FROM session_execution_scopes WHERE session_id=?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)
}

fn resolve(
    database: &rusqlite::Transaction<'_>,
    namespace: &str,
    binding: &SessionBinding,
    known_sessions: &BTreeMap<String, SessionBinding>,
    visiting: &mut BTreeSet<String>,
) -> Result<String, HarnessError> {
    binding.validate()?;
    let existing = database.query_row(
        "SELECT parent_session_id, is_subagent, execution_scope FROM session_execution_scopes WHERE session_id=?1",
        [&binding.session_id],
        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, bool>(1)?, row.get::<_, String>(2)?)),
    ).optional().map_err(database_error)?;
    if let Some((parent, is_subagent, scope)) = existing {
        if parent != binding.parent_session_id || is_subagent != binding.is_subagent {
            return Err(HarnessError::conflict(
                "session execution scope ancestry cannot change",
            ));
        }
        return Ok(scope);
    }
    if !visiting.insert(binding.session_id.clone()) {
        return Err(HarnessError::invalid(
            "session execution scope ancestry contains a cycle",
        ));
    }
    let inherited = if binding.is_subagent {
        match &binding.parent_session_id {
            Some(parent) => match known_sessions.get(parent) {
                Some(parent) => Some(resolve(
                    database,
                    namespace,
                    parent,
                    known_sessions,
                    visiting,
                )?),
                None => lookup_scope(database, parent)?,
            },
            None => None,
        }
    } else {
        None
    };
    let scope = inherited.unwrap_or_else(|| format!("local:{namespace}:{}", binding.session_id));
    database.execute(
        "INSERT INTO session_execution_scopes (session_id, parent_session_id, is_subagent, execution_scope) VALUES (?1, ?2, ?3, ?4)",
        params![binding.session_id, binding.parent_session_id, binding.is_subagent, scope],
    ).map_err(database_error)?;
    visiting.remove(&binding.session_id);
    Ok(scope)
}

#[cfg(test)]
mod tests;
