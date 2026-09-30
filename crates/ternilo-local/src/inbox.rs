use std::{collections::BTreeMap, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use ternilo_protocol::{
    HarnessError, InputAuthor, QueueEditRequest, SessionSubmission, SubmissionId,
    SubmissionPlacement,
};
use tokio::sync::{Mutex, broadcast};

use crate::{
    LocalInvalidationCategory, LocalInvalidationNotification, notifications::publish_invalidation,
};

mod execution_scopes;
mod uploads;

use tokio_rusqlite::{
    Connection, params,
    rusqlite::{self, OptionalExtension},
};

const INBOX_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxDocument {
    schema_version: u32,
    paused: bool,
    error: Option<String>,
    items: Vec<SessionSubmission>,
}

impl Default for InboxDocument {
    fn default() -> Self {
        Self {
            schema_version: INBOX_VERSION,
            paused: false,
            error: None,
            items: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InboxState {
    pub paused: bool,
    pub error: Option<String>,
    pub items: Vec<SessionSubmission>,
}

pub(crate) struct LocalInboxStore {
    connection: Connection,
    pub(crate) stream_id: String,
    locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    invalidations: broadcast::Sender<LocalInvalidationNotification>,
}

impl LocalInboxStore {
    pub(crate) async fn close(&self) -> Result<(), HarnessError> {
        self.connection
            .clone()
            .close()
            .await
            .map_err(|error| HarnessError::execution(format!("close local database: {error}")))
    }

    pub(crate) async fn open(
        path: &Path,
        invalidations: broadcast::Sender<LocalInvalidationNotification>,
    ) -> Result<Self, HarnessError> {
        let connection = Connection::open(path).await.map_err(|error| {
            HarnessError::execution(format!("open session inbox database: {error}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|error| {
                    HarnessError::execution(format!("secure session inbox database: {error}"))
                })?;
        }
        let stream_id = connection
            .call(|database| -> Result<String, HarnessError> {
                database
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .map_err(database_error)?;
                database
                    .pragma_update(None, "journal_mode", "WAL")
                    .map_err(database_error)?;
                database
                    .pragma_update(None, "synchronous", "FULL")
                    .map_err(database_error)?;
                database
                    .execute_batch(uploads::SCHEMA)
                    .map_err(database_error)?;
                database
                    .execute_batch(execution_scopes::SCHEMA)
                    .map_err(database_error)?;
                database
                    .query_row(
                        "SELECT stream_id FROM upload_stream WHERE singleton=1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(database_error)
            })
            .await
            .map_err(worker_error)?;
        Ok(Self {
            connection,
            stream_id,
            locks: Mutex::new(BTreeMap::new()),
            invalidations,
        })
    }

    async fn call<T, F>(&self, operation: F) -> Result<T, HarnessError>
    where
        T: Send + 'static,
        F: FnOnce(&mut rusqlite::Connection) -> Result<T, HarnessError> + Send + 'static,
    {
        self.connection.call(operation).await.map_err(worker_error)
    }

    async fn session_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        Arc::clone(
            locks
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    async fn load_unlocked(&self, session_id: &str) -> Result<InboxDocument, HarnessError> {
        let session_id = session_id.to_owned();
        self.call(move |database| load_document(database, &session_id))
            .await
    }

    async fn persist_unlocked(
        &self,
        session_id: &str,
        document: &InboxDocument,
    ) -> Result<(), HarnessError> {
        let id = session_id.to_owned();
        let document = document.clone();
        self.call(move |database| persist_document(database, &id, &document))
            .await?;
        self.notify(session_id);
        Ok(())
    }

    fn notify(&self, session_id: &str) {
        publish_invalidation(
            &self.invalidations,
            Some(session_id),
            LocalInvalidationCategory::Inbox,
            None,
        );
    }

    pub(crate) async fn snapshot(&self, session_id: &str) -> Result<InboxState, HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let document = self.load_unlocked(session_id).await?;
        Ok(InboxState {
            paused: document.paused,
            error: document.error,
            items: document.items,
        })
    }

    pub(crate) async fn enqueue(
        &self,
        session_id: &str,
        item: SessionSubmission,
    ) -> Result<(), HarnessError> {
        item.validate()?;
        if item.placement != SubmissionPlacement::Queued {
            return Err(HarnessError::invalid(
                "new submissions must enter the queued placement",
            ));
        }
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        if document.items.iter().any(|current| current.id == item.id) {
            return Err(HarnessError::invalid(format!(
                "submission {:?} already exists",
                item.id.as_str()
            )));
        }
        if item.content.regeneration_target().is_some() && !document.items.is_empty() {
            return Err(HarnessError::conflict(
                "clear queued inputs before replacing a turn",
            ));
        }
        document.paused = false;
        document.error = None;
        let uploads = uploads::from_submission(&item);
        document.items.push(item);
        let id = session_id.to_owned();
        self.call(move |database| {
            let transaction = database.transaction().map_err(database_error)?;
            persist_document(&transaction, &id, &document)?;
            uploads::insert_uploads(&transaction, &id, &uploads)?;
            transaction.commit().map_err(database_error)
        })
        .await?;
        self.notify(session_id);
        Ok(())
    }

    pub(crate) async fn edit(
        &self,
        session_id: &str,
        item_id: &SubmissionId,
        request: QueueEditRequest,
        updated_at_ms: u64,
    ) -> Result<SessionSubmission, HarnessError> {
        request.validate()?;
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let id = session_id.to_owned();
        let item_id = item_id.clone();
        let updated = self.call(move |database| {
            let transaction = database.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(database_error)?;
            let mut document = load_document(&transaction, &id)?;
            let item = mutable_queued_item(&mut document, &item_id)?;
            if item.updated_at_ms != request.expected_updated_at_ms {
                return Err(HarnessError::conflict("queued submission has been modified; load the latest version before editing"));
            }
            match &mut item.content {
                ternilo_protocol::SubmissionContent::Prompt { input }
                | ternilo_protocol::SubmissionContent::Skill { input, .. }
                | ternilo_protocol::SubmissionContent::Regenerate { input, .. } => {
                    *input = request.input;
                }
            }
            item.updated_at_ms = next_submission_timestamp(item.updated_at_ms, updated_at_ms)?;
            let updated = item.clone();
            persist_document(&transaction, &id, &document)?;
            transaction.commit().map_err(database_error)?;
            Ok(updated)
        }).await?;
        self.notify(session_id);
        Ok(updated)
    }

    pub(crate) async fn remove_queued(
        &self,
        session_id: &str,
        item_id: &SubmissionId,
    ) -> Result<SessionSubmission, HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        let index = document
            .items
            .iter()
            .position(|item| &item.id == item_id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown submission {item_id}")))?;
        if document.items[index].placement != SubmissionPlacement::Queued {
            return Err(HarnessError::invalid(
                "only queued submissions can be removed",
            ));
        }
        let removed = document.items.remove(index);
        self.persist_unlocked(session_id, &document).await?;
        Ok(removed)
    }

    #[cfg(test)]
    pub(crate) async fn move_queued(
        &self,
        session_id: &str,
        item_id: &SubmissionId,
        placement: SubmissionPlacement,
        updated_at_ms: u64,
    ) -> Result<SessionSubmission, HarnessError> {
        if !matches!(
            placement,
            SubmissionPlacement::Steering | SubmissionPlacement::Running
        ) {
            return Err(HarnessError::invalid(
                "queued submission may only move to steering or running",
            ));
        }
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        if document.paused && placement == SubmissionPlacement::Running {
            return Err(HarnessError::cancelled("session queue is paused"));
        }
        let item = mutable_queued_item(&mut document, item_id)?;
        item.placement = placement;
        if placement == SubmissionPlacement::Steering
            && item.content.regeneration_target().is_some()
        {
            return Err(HarnessError::invalid(
                "regeneration cannot be injected into an active turn",
            ));
        }
        item.updated_at_ms = next_submission_timestamp(item.updated_at_ms, updated_at_ms)?;
        let moved = item.clone();
        self.persist_unlocked(session_id, &document).await?;
        Ok(moved)
    }

    pub(crate) async fn requeue(
        &self,
        session_id: &str,
        item_id: &SubmissionId,
        updated_at_ms: u64,
    ) -> Result<(), HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        let item = document
            .items
            .iter_mut()
            .find(|item| &item.id == item_id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown submission {item_id}")))?;
        item.placement = SubmissionPlacement::Queued;
        item.updated_at_ms = next_submission_timestamp(item.updated_at_ms, updated_at_ms)?;
        self.persist_unlocked(session_id, &document).await
    }

    pub(crate) async fn first_queued(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionSubmission>, HarnessError> {
        Ok(self
            .snapshot(session_id)
            .await?
            .items
            .into_iter()
            .find(|item| item.placement == SubmissionPlacement::Queued))
    }

    pub(crate) async fn claim_batch(
        &self,
        session_id: &str,
        updated_at_ms: u64,
    ) -> Result<Vec<SessionSubmission>, HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        if document.paused {
            return Ok(Vec::new());
        }
        let mut batch = Vec::new();
        for item in &mut document.items {
            if item.placement != SubmissionPlacement::Queued {
                continue;
            }
            let conversational = matches!(&item.content,
                ternilo_protocol::SubmissionContent::Prompt { input } if !input.trim_start().starts_with('/'));
            if let Some(first) = batch.first()
                && (!conversational || !same_batch_author(first, item))
            {
                break;
            }
            item.placement = SubmissionPlacement::Running;
            item.updated_at_ms = next_submission_timestamp(item.updated_at_ms, updated_at_ms)?;
            batch.push(item.clone());
            if !conversational {
                break;
            }
        }
        self.persist_unlocked(session_id, &document).await?;
        Ok(batch)
    }

    pub(crate) async fn resume(&self, session_id: &str) -> Result<(), HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        document.paused = false;
        document.error = None;
        self.persist_unlocked(session_id, &document).await
    }

    pub(crate) async fn finish(
        &self,
        session_id: &str,
        item_id: &SubmissionId,
    ) -> Result<(), HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        document.items.retain(|item| &item.id != item_id);
        document.error = None;
        self.persist_unlocked(session_id, &document).await
    }

    pub(crate) async fn pause(&self, session_id: &str) -> Result<(), HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        document.paused = true;
        self.persist_unlocked(session_id, &document).await
    }

    pub(crate) async fn record_error(
        &self,
        session_id: &str,
        message: String,
    ) -> Result<(), HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        document.paused = true;
        document.error = Some(message);
        self.persist_unlocked(session_id, &document).await
    }

    pub(crate) async fn recover(
        &self,
        session_id: &str,
        consumed: &[SubmissionId],
    ) -> Result<bool, HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        document
            .items
            .retain(|item| !consumed.iter().any(|id| id == &item.id));
        for item in &mut document.items {
            if item.placement != SubmissionPlacement::Queued {
                item.placement = SubmissionPlacement::Queued;
                item.updated_at_ms =
                    next_submission_timestamp(item.updated_at_ms, item.updated_at_ms)?;
            }
        }
        let pending = !document.items.is_empty();
        self.persist_unlocked(session_id, &document).await?;
        Ok(pending)
    }

    pub(crate) async fn settle_run(
        &self,
        session_id: &str,
        consumed: &[SubmissionId],
        cancelled: bool,
        updated_at_ms: u64,
    ) -> Result<(), HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut document = self.load_unlocked(session_id).await?;
        document
            .items
            .retain(|item| !consumed.iter().any(|id| id == &item.id));
        for item in &mut document.items {
            if item.placement != SubmissionPlacement::Queued {
                item.placement = SubmissionPlacement::Queued;
                item.updated_at_ms = next_submission_timestamp(item.updated_at_ms, updated_at_ms)?;
            }
        }
        if cancelled {
            document.paused = true;
        }
        self.persist_unlocked(session_id, &document).await
    }

    pub(crate) async fn remove_session(&self, session_id: &str) -> Result<(), HarnessError> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let id = session_id.to_owned();
        self.call(move |database| {
            let transaction = database.transaction().map_err(database_error)?;
            transaction
                .execute("DELETE FROM session_inboxes WHERE session_id=?1", [&id])
                .map_err(database_error)?;
            transaction
                .execute("DELETE FROM accepted_uploads WHERE session_id=?1", [&id])
                .map_err(database_error)?;
            uploads::insert_change(
                &transaction,
                &id,
                &ternilo_protocol::AcceptedUploadChangeKind::SessionDeleted,
            )?;
            transaction.commit().map_err(database_error)
        })
        .await?;
        self.notify(session_id);
        Ok(())
    }
}

fn same_batch_author(first: &SessionSubmission, next: &SessionSubmission) -> bool {
    match (
        first.provenance.as_ref().map(|input| &input.author),
        next.provenance.as_ref().map(|input| &input.author),
    ) {
        (Some(InputAuthor::Local), Some(InputAuthor::Local)) => true,
        (
            Some(InputAuthor::Account { user_id: first, .. }),
            Some(InputAuthor::Account { user_id: next, .. }),
        ) => first == next,
        // Automation source categories do not identify their initiating actor.
        _ => false,
    }
}

fn load_document(
    database: &rusqlite::Connection,
    session_id: &str,
) -> Result<InboxDocument, HarnessError> {
    let json: Option<String> = database
        .query_row(
            "SELECT document FROM session_inboxes WHERE session_id=?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    let document = json.map_or_else(
        || Ok(InboxDocument::default()),
        |json| {
            serde_json::from_str(&json)
                .map_err(|error| HarnessError::execution(format!("decode session inbox: {error}")))
        },
    )?;
    validate_document(&document)?;
    Ok(document)
}

fn persist_document(
    database: &rusqlite::Connection,
    session_id: &str,
    document: &InboxDocument,
) -> Result<(), HarnessError> {
    uploads::require_live_session_id(database, session_id)?;
    let json = serde_json::to_string(document)
        .map_err(|error| HarnessError::execution(format!("encode session inbox: {error}")))?;
    database.execute("INSERT INTO session_inboxes (session_id,document) VALUES (?1,?2) ON CONFLICT(session_id) DO UPDATE SET document=excluded.document", params![session_id,json])
        .map_err(database_error)?;
    Ok(())
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used directly by Result::map_err."
)]
fn database_error(error: rusqlite::Error) -> HarnessError {
    HarnessError::execution(format!("session inbox database: {error}"))
}

fn worker_error(error: tokio_rusqlite::Error<HarnessError>) -> HarnessError {
    match error {
        tokio_rusqlite::Error::Error(error) => error,
        other => HarnessError::execution(format!("session inbox database worker: {other}")),
    }
}

fn mutable_queued_item<'a>(
    document: &'a mut InboxDocument,
    item_id: &SubmissionId,
) -> Result<&'a mut SessionSubmission, HarnessError> {
    let item = document
        .items
        .iter_mut()
        .find(|item| &item.id == item_id)
        .ok_or_else(|| HarnessError::invalid(format!("unknown submission {item_id}")))?;
    if item.placement != SubmissionPlacement::Queued {
        return Err(HarnessError::invalid(
            "only queued submissions can be changed",
        ));
    }
    Ok(item)
}

fn next_submission_timestamp(previous: u64, now: u64) -> Result<u64, HarnessError> {
    previous
        .checked_add(1)
        .map(|next| now.max(next))
        .ok_or_else(|| HarnessError::execution("submission timestamp exhausted"))
}

fn validate_document(document: &InboxDocument) -> Result<(), HarnessError> {
    if document.schema_version != INBOX_VERSION {
        return Err(HarnessError::execution(format!(
            "unsupported inbox version {}; expected {INBOX_VERSION}",
            document.schema_version
        )));
    }
    for (index, item) in document.items.iter().enumerate() {
        item.validate()?;
        if document.items[..index]
            .iter()
            .any(|current| current.id == item.id || current.run_id == item.run_id)
        {
            return Err(HarnessError::execution(
                "session inbox contains duplicate ids",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ternilo_protocol::{
        InputProvenance, RunId, SessionSubmission, SubmissionContent, SubmissionId,
        SubmissionPlacement,
    };

    use super::*;

    mod batch_authors;
    mod edit_conflicts;

    fn item(id: &str) -> SessionSubmission {
        SessionSubmission {
            provenance: None,
            id: SubmissionId::new(id),
            run_id: RunId::new(format!("run-{id}")),
            content: SubmissionContent::Prompt {
                input: id.to_owned(),
            },
            references: Vec::new(),
            attachments: Vec::new(),
            placement: SubmissionPlacement::Queued,
            created_at_ms: 10,
            updated_at_ms: 10,
        }
    }

    fn authored_item(id: &str, author: InputAuthor) -> SessionSubmission {
        let mut item = item(id);
        item.provenance = Some(InputProvenance {
            input_id: item.id.clone(),
            run_id: Some(item.run_id.clone()),
            author,
        });
        item
    }

    async fn test_store(path: std::path::PathBuf) -> LocalInboxStore {
        let (invalidations, _) = broadcast::channel(16);
        LocalInboxStore::open(&path.join("inbox.sqlite3"), invalidations)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn batch_claims_are_durable_and_only_consumed_inputs_are_removed() {
        let directory = tempfile::tempdir().unwrap();
        let store = test_store(directory.path().to_owned()).await;
        for name in ["B", "C"] {
            store
                .enqueue("session-a", authored_item(name, InputAuthor::Local))
                .await
                .unwrap();
        }
        let batch = store.claim_batch("session-a", 11).await.unwrap();
        assert_eq!(
            batch
                .iter()
                .map(|item| item.content.input())
                .collect::<Vec<_>>(),
            ["B", "C"]
        );
        store
            .enqueue("session-a", authored_item("D", InputAuthor::Local))
            .await
            .unwrap();
        store
            .settle_run("session-a", &[SubmissionId::new("B")], true, 12)
            .await
            .unwrap();
        drop(store);
        let reopened = test_store(directory.path().to_owned()).await;
        let pending = reopened.snapshot("session-a").await.unwrap();
        assert!(pending.paused);
        assert_eq!(
            pending
                .items
                .iter()
                .map(|item| item.content.input())
                .collect::<Vec<_>>(),
            ["C", "D"]
        );
        assert!(
            reopened
                .claim_batch("session-a", 13)
                .await
                .unwrap()
                .is_empty()
        );
        reopened.resume("session-a").await.unwrap();
        assert_eq!(
            reopened.claim_batch("session-a", 14).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn persists_exact_occurrence_mutations_and_cancel_pause() {
        let directory = tempfile::tempdir().unwrap();
        let store = test_store(directory.path().to_owned()).await;
        store.enqueue("session-a", item("one")).await.unwrap();
        store.enqueue("session-a", item("two")).await.unwrap();
        store
            .edit(
                "session-a",
                &SubmissionId::new("two"),
                QueueEditRequest {
                    input: "edited".to_owned(),
                    expected_updated_at_ms: 10,
                },
                11,
            )
            .await
            .unwrap();
        store.pause("session-a").await.unwrap();
        assert!(
            store
                .move_queued(
                    "session-a",
                    &SubmissionId::new("one"),
                    SubmissionPlacement::Running,
                    12,
                )
                .await
                .unwrap_err()
                .is_cancelled()
        );
        let snapshot = store.snapshot("session-a").await.unwrap();
        assert!(snapshot.paused);
        assert_eq!(snapshot.items[1].content.input(), "edited");

        let reopened = test_store(directory.path().to_owned()).await;
        assert_eq!(reopened.snapshot("session-a").await.unwrap(), snapshot);
        reopened
            .remove_queued("session-a", &SubmissionId::new("one"))
            .await
            .unwrap();
        assert_eq!(reopened.snapshot("session-a").await.unwrap().items.len(), 1);
    }

    #[tokio::test]
    async fn recovery_removes_durable_handoffs_and_requeues_transient_placements() {
        let directory = tempfile::tempdir().unwrap();
        let store = test_store(directory.path().to_owned()).await;
        store.enqueue("session-a", item("done")).await.unwrap();
        store.enqueue("session-a", item("pending")).await.unwrap();
        store
            .move_queued(
                "session-a",
                &SubmissionId::new("done"),
                SubmissionPlacement::Steering,
                11,
            )
            .await
            .unwrap();
        store
            .move_queued(
                "session-a",
                &SubmissionId::new("pending"),
                SubmissionPlacement::Running,
                11,
            )
            .await
            .unwrap();
        store.pause("session-a").await.unwrap();
        assert!(
            store
                .recover("session-a", &[SubmissionId::new("done")])
                .await
                .unwrap()
        );
        let snapshot = store.snapshot("session-a").await.unwrap();
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].id, SubmissionId::new("pending"));
        assert_eq!(snapshot.items[0].placement, SubmissionPlacement::Queued);
        assert!(
            snapshot.paused,
            "cancellation pause survives restart recovery"
        );
    }

    #[tokio::test]
    async fn accepted_uploads_are_atomic_immutable_and_outlive_the_queue() {
        use ternilo_protocol::{AcceptedUploadChangeKind, Attachment};
        let directory = tempfile::tempdir().unwrap();
        let store = test_store(directory.path().to_owned()).await;
        let stream_id = store.stream_id.clone();
        let mut first = item("upload-one");
        first.attachments.push(Attachment {
            name: "same.txt".to_owned(),
            media_type: "text/plain".to_owned(),
            content: "immutable original".to_owned(),
        });
        store.enqueue("session-a", first.clone()).await.unwrap();
        let accepted = store.accepted_uploads("session-a").await.unwrap();
        assert_eq!(accepted.len(), 1);
        store.remove_queued("session-a", &first.id).await.unwrap();
        assert_eq!(store.accepted_uploads("session-a").await.unwrap(), accepted);
        let mut replacement = first.clone();
        replacement.attachments[0].content = "attempted replacement".to_owned();
        assert_eq!(
            store
                .enqueue("session-a", replacement)
                .await
                .unwrap_err()
                .code,
            ternilo_protocol::ErrorCode::InvalidInput,
            "invalid upload replacements retain their application error through the SQLite worker"
        );
        assert!(
            store.snapshot("session-a").await.unwrap().items.is_empty(),
            "failed retention rolls back queue insertion"
        );
        let changes = store.upload_changes(None, 1).await.unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            changes[0].kind,
            AcceptedUploadChangeKind::UploadAccepted { .. }
        ));
        assert!(
            !serde_json::to_string(&changes)
                .unwrap()
                .contains("immutable original")
        );
        assert!(
            store
                .upload_changes(Some(changes[0].seq), 1)
                .await
                .unwrap()
                .is_empty()
        );
        drop(store);
        let store = test_store(directory.path().to_owned()).await;
        assert_eq!(store.stream_id, stream_id);
        assert_eq!(store.accepted_uploads("session-a").await.unwrap(), accepted);
        store.remove_session("session-a").await.unwrap();
        assert!(
            store
                .accepted_uploads("session-a")
                .await
                .unwrap()
                .is_empty()
        );
        let deleted = store.upload_changes(Some(changes[0].seq), 1).await.unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].seq, changes[0].seq + 1);
        assert_eq!(deleted[0].kind, AcceptedUploadChangeKind::SessionDeleted);
        assert_eq!(
            store.enqueue("session-a", first).await.unwrap_err().code,
            ternilo_protocol::ErrorCode::Conflict,
            "deleted session identities retain their conflict error through the SQLite worker"
        );
        assert!(store.retain_uploads("session-a", accepted).await.is_err());
        assert!(store.snapshot("session-a").await.unwrap().items.is_empty());
        assert!(
            store
                .upload_changes(Some(deleted[0].seq), 1)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
