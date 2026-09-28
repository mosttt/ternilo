use ternilo_protocol::{
    AcceptedSessionUpload, FileSourceStatus, RunId, SessionEvent, SessionFileItem, SessionFileKind,
    SessionFileQuery, SessionId, WorkspaceId, session_file_references,
};

use super::{
    HarnessError, LocalSearchIndex, SqlValue, params, params_from_iter, rusqlite, sqlite_error,
    to_i64,
};

pub(super) const SCHEMA: &str = "
DROP TABLE IF EXISTS session_files;
CREATE TABLE session_files (
    session_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    event_seq INTEGER,
    attachment_index INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    media_type TEXT NOT NULL,
    path TEXT,
    PRIMARY KEY (session_id, file_id)
);
CREATE INDEX IF NOT EXISTS session_files_page
ON session_files (occurred_at_ms DESC, session_id DESC, file_id DESC);
CREATE INDEX IF NOT EXISTS session_files_workspace
ON session_files (workspace_id, occurred_at_ms DESC);
";

pub(super) fn documents(
    session_id: &str,
    workspace_id: &str,
    events: &[SessionEvent],
) -> Vec<SessionFileItem> {
    events
        .iter()
        .flat_map(|event| {
            session_file_references(event)
                .into_iter()
                .map(move |file| SessionFileItem {
                    id: file.id,
                    session_id: SessionId::new(session_id),
                    session_title: String::new(),
                    session_archived: false,
                    workspace_id: WorkspaceId::new(workspace_id),
                    workspace_name: String::new(),
                    kind: file.kind,
                    name: file.attachment.name.clone(),
                    media_type: file.attachment.media_type.clone(),
                    path: file.path.map(str::to_owned),
                    occurred_at_ms: file.occurred_at_ms,
                    event_seq: file.event_seq,
                    run_id: event.run_id.clone(),
                    source_status: FileSourceStatus::Online,
                    attachment_index: file.attachment_index,
                })
        })
        .collect()
}

pub(super) fn upload_documents(
    session_id: &str,
    workspace_id: &str,
    uploads: &[AcceptedSessionUpload],
) -> Vec<SessionFileItem> {
    uploads
        .iter()
        .map(|upload| SessionFileItem {
            id: upload.file_id(),
            session_id: SessionId::new(session_id),
            session_title: String::new(),
            session_archived: false,
            workspace_id: WorkspaceId::new(workspace_id),
            workspace_name: String::new(),
            kind: SessionFileKind::Upload,
            name: upload.attachment.name.clone(),
            media_type: upload.attachment.media_type.clone(),
            path: None,
            occurred_at_ms: upload.created_at_ms,
            event_seq: None,
            run_id: upload.submitted_run_id.clone(),
            source_status: FileSourceStatus::Online,
            attachment_index: upload.attachment_index,
        })
        .collect()
}

pub(super) fn insert(
    database: &rusqlite::Transaction<'_>,
    files: &[SessionFileItem],
) -> Result<(), HarnessError> {
    let mut statement = database.prepare(
        "INSERT INTO session_files (session_id,workspace_id,event_seq,attachment_index,run_id,occurred_at_ms,kind,name,media_type,path,file_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(session_id,file_id) DO UPDATE SET
         event_seq=COALESCE(excluded.event_seq,session_files.event_seq),
         workspace_id=excluded.workspace_id,
         run_id=CASE WHEN excluded.file_id LIKE 'submission-%' AND excluded.event_seq IS NOT NULL
                    THEN session_files.run_id ELSE excluded.run_id END,
         occurred_at_ms=excluded.occurred_at_ms,
         kind=excluded.kind,name=excluded.name,media_type=excluded.media_type,path=excluded.path"
    ).map_err(sqlite_error)?;
    for item in files {
        statement
            .execute(params![
                item.session_id.as_str(),
                item.workspace_id.as_str(),
                item.event_seq
                    .map(|seq| to_i64(seq, "file event sequence"))
                    .transpose()?,
                i64::from(item.attachment_index),
                item.run_id.as_str(),
                to_i64(item.occurred_at_ms, "file timestamp")?,
                item.kind.as_str(),
                item.name,
                item.media_type,
                item.path,
                item.id,
            ])
            .map_err(sqlite_error)?;
    }
    Ok(())
}

impl LocalSearchIndex {
    pub(crate) async fn append_uploads_fail_soft(
        &self,
        session_id: String,
        workspace_id: String,
        uploads: Vec<AcceptedSessionUpload>,
    ) {
        let files = upload_documents(&session_id, &workspace_id, &uploads);
        let result = self
            .call(move |database| {
                let transaction = database.transaction().map_err(sqlite_error)?;
                insert(&transaction, &files)?;
                transaction.commit().map_err(sqlite_error)
            })
            .await;
        if result.is_err() {
            self.dirty.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    pub(crate) async fn file_event_seq(
        &self,
        session_id: &str,
        file_id: &str,
    ) -> Result<Option<u64>, HarnessError> {
        use rusqlite::OptionalExtension as _;
        let session_id = session_id.to_owned();
        let file_id = file_id.to_owned();
        self.call(move |database| {
            database
                .query_row(
                    "SELECT event_seq FROM session_files WHERE session_id=?1 AND file_id=?2",
                    params![session_id, file_id],
                    |row| row.get::<_, Option<u64>>(0),
                )
                .optional()
                .map(Option::flatten)
                .map_err(sqlite_error)
        })
        .await
    }

    pub(crate) async fn files(
        &self,
        request: &SessionFileQuery,
        limit: usize,
    ) -> Result<Vec<SessionFileItem>, HarnessError> {
        request.validate()?;
        let cursor = request.parsed_cursor()?;
        let request = request.clone();
        self.call(move |database| {
            let mut sql = String::from("SELECT session_id,workspace_id,event_seq,attachment_index,run_id,occurred_at_ms,kind,name,media_type,path,file_id FROM session_files WHERE 1=1");
            let mut parameters = Vec::new();
            if let Some(id) = request.session_id {
                sql.push_str(" AND session_id=?");
                parameters.push(SqlValue::Text(id.as_str().to_owned()));
            }
            if let Some(id) = request.workspace_id {
                sql.push_str(" AND workspace_id=?");
                parameters.push(SqlValue::Text(id.as_str().to_owned()));
            }
            if let Some(kind) = request.kind {
                sql.push_str(" AND kind=?");
                parameters.push(SqlValue::Text(kind.as_str().to_owned()));
            }
            if let Some(query) = request.query.filter(|value| !value.trim().is_empty()) {
                sql.push_str(" AND (instr(lower(name),?)>0 OR instr(lower(COALESCE(path,'')),?)>0)");
                let query = query.trim().to_lowercase();
                parameters.push(SqlValue::Text(query.clone()));
                parameters.push(SqlValue::Text(query));
            }
            if let Some(cursor) = cursor {
                sql.push_str(" AND (occurred_at_ms,session_id,file_id)<(?,?,?)");
                parameters.extend([
                    SqlValue::Integer(to_i64(cursor.occurred_at_ms,"file cursor timestamp")?),
                    SqlValue::Text(cursor.session_id.as_str().to_owned()),
                    SqlValue::Text(cursor.file_id),
                ]);
            }
            sql.push_str(" ORDER BY occurred_at_ms DESC,session_id DESC,file_id DESC LIMIT ?");
            parameters.push(SqlValue::Integer(i64::try_from(limit).map_err(|_| HarnessError::invalid("file page limit exceeds database range"))?));
            let mut statement = database.prepare(&sql).map_err(sqlite_error)?;
            let rows = statement.query_map(params_from_iter(parameters), |row| {
                let kind: String = row.get(6)?;
                let kind = if kind == "upload" { SessionFileKind::Upload } else { SessionFileKind::Generated };
                let event_seq = row.get::<_,Option<u64>>(2)?;
                let attachment_index = row.get::<_,u32>(3)?;
                Ok(SessionFileItem {
                    id: row.get(10)?,
                    session_id: SessionId::new(row.get::<_,String>(0)?),
                    session_title: String::new(),
                    session_archived: false,
                    workspace_id: WorkspaceId::new(row.get::<_,String>(1)?),
                    workspace_name: String::new(),
                    event_seq,
                    attachment_index,
                    run_id: RunId::new(row.get::<_,String>(4)?),
                    occurred_at_ms: row.get(5)?,
                    kind,
                    name: row.get(7)?,
                    media_type: row.get(8)?,
                    path: row.get(9)?,
                    source_status: FileSourceStatus::Online,
                })
            }).map_err(sqlite_error)?;
            rows.map(|row| row.map_err(sqlite_error)).collect()
        }).await
    }
}
