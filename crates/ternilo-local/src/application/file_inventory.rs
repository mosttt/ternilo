use base64::{Engine as _, engine::general_purpose::STANDARD};
use ternilo_protocol::{
    SessionFileContent, SessionFileLocator, SessionFilePage, SessionFileQuery,
    session_file_references,
};

use super::{BTreeMap, HarnessError, JsonlEventStore, LocalApplication};

impl LocalApplication {
    pub async fn files(&self, query: SessionFileQuery) -> Result<SessionFilePage, HarnessError> {
        query.validate()?;
        let snapshot = self.state.snapshot().await;
        if query.session_id.as_ref().is_some_and(|id| {
            !snapshot
                .sessions
                .iter()
                .any(|session| &session.identity.session_id == id)
        }) {
            return Err(HarnessError::invalid("unknown file session"));
        }
        let index = self.session_archive.index();
        index.refresh_if_dirty(&self.state).await?;
        let sessions = snapshot
            .sessions
            .iter()
            .map(|session| (&session.identity.session_id, session))
            .collect::<BTreeMap<_, _>>();
        let workspaces = snapshot
            .workspaces
            .iter()
            .map(|workspace| (&workspace.workspace_id, workspace))
            .collect::<BTreeMap<_, _>>();
        let mut items = index.files(&query, usize::from(query.limit) + 1).await?;
        items.retain_mut(|item| {
            let Some(session) = sessions.get(&item.session_id) else {
                return false;
            };
            if session.workspace_id != item.workspace_id {
                return false;
            }
            item.session_title.clone_from(&session.title);
            item.session_archived = session.archived_at_ms.is_some();
            item.workspace_name = workspaces.get(&item.workspace_id).map_or_else(
                || "Ungrouped".to_owned(),
                |workspace| workspace.title.clone(),
            );
            true
        });
        let next_cursor = if items.len() > usize::from(query.limit) {
            items.truncate(usize::from(query.limit));
            items
                .last()
                .map(|item| item.cursor().encode())
                .transpose()?
        } else {
            None
        };
        Ok(SessionFilePage {
            items,
            next_cursor,
            offline_sources: Vec::new(),
        })
    }

    pub async fn session_file_content(
        &self,
        session_id: &str,
        file_id: &str,
    ) -> Result<SessionFileContent, HarnessError> {
        let locator = SessionFileLocator::parse(file_id)?;
        let session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid("unknown file session"))?;
        let attachment = match locator {
            SessionFileLocator::Submission { .. } => {
                if let Some(upload) = self.inbox.accepted_upload(session_id, file_id).await? {
                    upload.attachment
                } else {
                    // Forked history can refer to an upload accepted by its parent.
                    let index = self.session_archive.index();
                    index.refresh_if_dirty(&self.state).await?;
                    let seq = index
                        .file_event_seq(session_id, file_id)
                        .await?
                        .ok_or_else(|| {
                            HarnessError::invalid("file does not exist in this session")
                        })?;
                    self.file_event_attachment(&session.identity.session_id, file_id, seq)
                        .await?
                }
            }
            SessionFileLocator::Event { event_seq, .. } => {
                self.file_event_attachment(&session.identity.session_id, file_id, event_seq)
                    .await?
            }
        };
        let bytes = if attachment.is_reference() {
            self.attachments.reference_bytes(&attachment).await?
        } else {
            crate::inline_file_attachment_bytes(&attachment)?
        };
        Ok(SessionFileContent {
            name: attachment.name.clone(),
            media_type: attachment.media_type.clone(),
            content_base64: STANDARD.encode(bytes),
        })
    }

    async fn file_event_attachment(
        &self,
        session_id: &ternilo_protocol::SessionId,
        file_id: &str,
        seq: u64,
    ) -> Result<ternilo_protocol::Attachment, HarnessError> {
        let event = JsonlEventStore::new(&self.state.sessions_dir(), session_id)
            .event_at(seq)
            .await?
            .ok_or_else(|| HarnessError::invalid("file event does not exist in this session"))?;
        session_file_references(&event)
            .into_iter()
            .find(|file| file.id == file_id)
            .map(|file| file.attachment.clone())
            .ok_or_else(|| HarnessError::invalid("file does not exist in this session"))
    }

    #[must_use]
    pub fn accepted_upload_stream_id(&self) -> &str {
        &self.inbox.stream_id
    }

    pub async fn accepted_upload_changes(
        &self,
        after_seq: Option<u64>,
        limit: u16,
    ) -> Result<Vec<ternilo_protocol::AcceptedUploadChange>, HarnessError> {
        self.inbox.upload_changes(after_seq, limit).await
    }
}

#[cfg(test)]
mod tests;
