use ternilo_protocol::{
    AcceptedSessionUpload, AcceptedUploadChange, AcceptedUploadChangeKind, SessionId,
    SessionSubmission,
};

use super::{
    HarnessError, LocalInboxStore, OptionalExtension as _, database_error, params, rusqlite,
};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS session_inboxes (
    session_id TEXT PRIMARY KEY,
    document TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS upload_stream (
    singleton INTEGER PRIMARY KEY CHECK (singleton=1),
    stream_id TEXT NOT NULL
);
INSERT OR IGNORE INTO upload_stream VALUES (1, lower(hex(randomblob(16))));
CREATE TABLE IF NOT EXISTS accepted_uploads (
    session_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    upload TEXT NOT NULL,
    PRIMARY KEY (session_id,file_id)
);
CREATE TABLE IF NOT EXISTS accepted_upload_changes (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    change TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS accepted_upload_deleted_sessions
ON accepted_upload_changes(session_id)
WHERE json_extract(change,'$.type')='session_deleted';
";

pub(super) fn from_submission(item: &SessionSubmission) -> Vec<AcceptedSessionUpload> {
    item.attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| AcceptedSessionUpload {
            submission_id: item.id.clone(),
            attachment_index: u32::try_from(index).expect("validated attachment count"),
            created_at_ms: item.created_at_ms,
            submitted_run_id: item.run_id.clone(),
            attachment: attachment.clone(),
        })
        .collect()
}

pub(super) fn insert_uploads(
    database: &rusqlite::Transaction<'_>,
    session_id: &str,
    uploads: &[AcceptedSessionUpload],
) -> Result<(), HarnessError> {
    require_live_session_id(database, session_id)?;
    for upload in uploads {
        upload.metadata().validate()?;
        upload.attachment.validate()?;
        let file_id = upload.file_id();
        let previous: Option<String> = database
            .query_row(
                "SELECT upload FROM accepted_uploads WHERE session_id=?1 AND file_id=?2",
                params![session_id, file_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        if let Some(previous) = previous {
            if decode::<AcceptedSessionUpload>(&previous)? != *upload {
                return Err(HarnessError::invalid(
                    "accepted upload identity cannot change",
                ));
            }
            continue;
        }
        database
            .execute(
                "INSERT INTO accepted_uploads (session_id,file_id,upload) VALUES (?1,?2,?3)",
                params![session_id, file_id, encode(upload)?],
            )
            .map_err(database_error)?;
        insert_change(
            database,
            session_id,
            &AcceptedUploadChangeKind::UploadAccepted {
                upload: upload.metadata(),
            },
        )?;
    }
    Ok(())
}

pub(super) fn require_live_session_id(
    database: &rusqlite::Connection,
    session_id: &str,
) -> Result<(), HarnessError> {
    let deleted: bool = database.query_row(
        "SELECT EXISTS(SELECT 1 FROM accepted_upload_changes WHERE session_id=?1 AND json_extract(change,'$.type')='session_deleted')",
        [session_id], |row| row.get(0),
    ).map_err(database_error)?;
    if deleted {
        return Err(HarnessError::conflict(
            "deleted session IDs cannot be reused",
        ));
    }
    Ok(())
}

pub(super) fn insert_change(
    database: &rusqlite::Transaction<'_>,
    session_id: &str,
    change: &AcceptedUploadChangeKind,
) -> Result<(), HarnessError> {
    database
        .execute(
            "INSERT INTO accepted_upload_changes (session_id,change) VALUES (?1,?2)",
            params![session_id, encode(change)?],
        )
        .map_err(database_error)?;
    Ok(())
}

impl LocalInboxStore {
    pub(crate) async fn require_live_session_id(
        &self,
        session_id: &str,
    ) -> Result<(), HarnessError> {
        let session_id = session_id.to_owned();
        self.call(move |database| require_live_session_id(database, &session_id))
            .await
    }

    pub(crate) async fn accepted_uploads(
        &self,
        session_id: &str,
    ) -> Result<Vec<AcceptedSessionUpload>, HarnessError> {
        let id = session_id.to_owned();
        self.call(move |database| {
            let mut statement = database
                .prepare("SELECT upload FROM accepted_uploads WHERE session_id=?1 ORDER BY file_id")
                .map_err(database_error)?;
            statement
                .query_map([id], |row| row.get::<_, String>(0))
                .map_err(database_error)?
                .map(|row| decode(&row.map_err(database_error)?))
                .collect()
        })
        .await
    }

    pub(crate) async fn accepted_upload(
        &self,
        session_id: &str,
        file_id: &str,
    ) -> Result<Option<AcceptedSessionUpload>, HarnessError> {
        let id = session_id.to_owned();
        let file_id = file_id.to_owned();
        self.call(move |database| {
            let json: Option<String> = database
                .query_row(
                    "SELECT upload FROM accepted_uploads WHERE session_id=?1 AND file_id=?2",
                    params![id, file_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database_error)?;
            json.as_deref().map(decode).transpose()
        })
        .await
    }

    pub(crate) async fn retain_uploads(
        &self,
        session_id: &str,
        uploads: Vec<AcceptedSessionUpload>,
    ) -> Result<(), HarnessError> {
        let id = session_id.to_owned();
        self.call(move |database| {
            let transaction = database.transaction().map_err(database_error)?;
            insert_uploads(&transaction, &id, &uploads)?;
            transaction.commit().map_err(database_error)
        })
        .await
    }

    pub(crate) async fn upload_changes(
        &self,
        after_seq: Option<u64>,
        limit: u16,
    ) -> Result<Vec<AcceptedUploadChange>, HarnessError> {
        if !(1..=200).contains(&limit) {
            return Err(HarnessError::invalid(
                "upload change limit must be between 1 and 200",
            ));
        }
        let after_seq = i64::try_from(after_seq.unwrap_or(0))
            .map_err(|_| HarnessError::invalid("invalid upload change cursor"))?;
        self.call(move |database| {
            let mut statement = database.prepare("SELECT seq,session_id,change FROM accepted_upload_changes WHERE seq>?1 ORDER BY seq LIMIT ?2").map_err(database_error)?;
            statement.query_map(params![after_seq,limit], |row| Ok((row.get::<_,u64>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?)))
                .map_err(database_error)?.map(|row| {
                    let (seq, session_id, json) = row.map_err(database_error)?;
                    Ok(AcceptedUploadChange { seq, session_id: SessionId::new(session_id), kind: decode(&json)? })
                }).collect()
        }).await
    }
}

fn encode(value: &impl serde::Serialize) -> Result<String, HarnessError> {
    serde_json::to_string(value)
        .map_err(|error| HarnessError::execution(format!("encode accepted upload: {error}")))
}

fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, HarnessError> {
    serde_json::from_str(value)
        .map_err(|error| HarnessError::execution(format!("decode accepted upload: {error}")))
}
