use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
};

use ternilo_kernel::{SessionArchive, SessionEventStore, SessionProjectionUnit};
use ternilo_protocol::{
    HarnessError, RunId, SessionEvent, SessionEventReadRequest, SessionId, SessionIdentity,
    SessionProjectionSnapshot, SessionSearchFilters, SessionSearchHit, SessionSearchRequest,
    SessionTrace, WorkspaceId,
};

use crate::{
    event_store::JsonlEventStore,
    projection_cache::LocalProjectionCache,
    search_index::LocalSearchIndex,
    state::{LocalSession, LocalState},
};

pub(crate) struct LocalSessionArchive {
    state: Arc<LocalState>,
    index: Arc<LocalSearchIndex>,
    projections: Arc<LocalProjectionCache>,
}

impl LocalSessionArchive {
    pub(crate) async fn open(
        state: Arc<LocalState>,
        inbox: Arc<crate::inbox::LocalInboxStore>,
    ) -> Result<Self, HarnessError> {
        let index = Arc::new(LocalSearchIndex::open(&state.search_index_path(), inbox).await?);
        index.rebuild(&state).await?;
        let projections =
            Arc::new(LocalProjectionCache::open(&state.projection_cache_path()).await?);
        Ok(Self {
            state,
            index,
            projections,
        })
    }

    pub(crate) fn index(&self) -> Arc<LocalSearchIndex> {
        Arc::clone(&self.index)
    }

    pub(crate) async fn delete_from_index(&self, session_id: &str) {
        self.index.delete_fail_soft(session_id.to_owned()).await;
        self.projections
            .delete_fail_soft(session_id.to_owned())
            .await;
    }

    pub(crate) async fn projection(
        &self,
        requester: SessionIdentity,
        session_id: SessionId,
        units: Vec<Arc<dyn SessionProjectionUnit>>,
    ) -> Result<SessionProjectionSnapshot, HarnessError> {
        session_id.validate()?;
        let session = self.authorized_session(&requester, &session_id).await?;
        self.projections
            .snapshot(&self.state, &session, units)
            .await
    }

    pub(crate) async fn checkpoint_fail_soft(
        &self,
        session: &LocalSession,
        units: Vec<Arc<dyn SessionProjectionUnit>>,
    ) {
        let _ = self.projections.snapshot(&self.state, session, units).await;
    }

    async fn authorized_session(
        &self,
        requester: &SessionIdentity,
        session_id: &SessionId,
    ) -> Result<LocalSession, HarnessError> {
        let session = self
            .state
            .session(session_id.as_str())
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        if session.identity.tenant_id != requester.tenant_id
            || session.identity.user_id != requester.user_id
        {
            return Err(HarnessError::policy(
                "session archive access is outside the caller scope",
            ));
        }
        Ok(session)
    }

    async fn load(&self, session_id: &SessionId) -> Result<Vec<SessionEvent>, HarnessError> {
        JsonlEventStore::new(&self.state.sessions_dir(), session_id)
            .load()
            .await
    }
}

impl SessionArchive for LocalSessionArchive {
    fn search<'a>(
        &'a self,
        requester: SessionIdentity,
        request: SessionSearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionSearchHit>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            request.validate()?;
            self.index.refresh_if_dirty(&self.state).await?;
            let needle = request.query.trim().to_lowercase();
            let mut sessions = self.state.snapshot().await.sessions;
            sessions.retain(|session| {
                session.identity.tenant_id == requester.tenant_id
                    && session.identity.user_id == requester.user_id
                    && request
                        .session_id
                        .as_ref()
                        .is_none_or(|id| id == &session.identity.session_id)
                    && request
                        .workspace_id
                        .as_ref()
                        .is_none_or(|id| id == &session.workspace_id)
            });
            sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at_ms));
            let by_id = sessions
                .iter()
                .map(|session| (session.identity.session_id.as_str(), session))
                .collect::<BTreeMap<_, _>>();
            let mut hits = Vec::new();
            if request.filters == SessionSearchFilters::default() {
                for session in &sessions {
                    if session.title.to_lowercase().contains(&needle) {
                        hits.push(SessionSearchHit {
                            session_id: session.identity.session_id.clone(),
                            workspace_id: session.workspace_id.clone(),
                            title: session.title.clone(),
                            updated_at_ms: session.updated_at_ms,
                            event_seq: None,
                            occurred_at_ms: None,
                            run_id: None,
                            category: None,
                            excerpt: session.title.clone(),
                        });
                    }
                    if hits.len() >= request.limit as usize {
                        return Ok(hits);
                    }
                }
            }

            let remaining = request.limit as usize - hits.len();
            for indexed in self.index.search(&request, remaining).await? {
                let Some(session) = by_id.get(indexed.session_id.as_str()) else {
                    continue;
                };
                if session.workspace_id.as_str() != indexed.workspace_id {
                    continue;
                }
                hits.push(SessionSearchHit {
                    session_id: SessionId::new(indexed.session_id),
                    workspace_id: WorkspaceId::new(indexed.workspace_id),
                    title: session.title.clone(),
                    updated_at_ms: session.updated_at_ms,
                    event_seq: Some(indexed.seq),
                    occurred_at_ms: Some(indexed.occurred_at_ms),
                    run_id: Some(RunId::new(indexed.run_id)),
                    category: Some(indexed.category),
                    excerpt: excerpt(&indexed.excerpt),
                });
            }
            Ok(hits)
        })
    }

    fn read_events<'a>(
        &'a self,
        requester: SessionIdentity,
        request: SessionEventReadRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            request.validate()?;
            self.authorized_session(&requester, &request.session_id)
                .await?;
            Ok(self
                .load(&request.session_id)
                .await?
                .into_iter()
                .filter(|event| event.seq >= request.start_seq)
                .take(request.limit as usize)
                .collect())
        })
    }

    fn trace<'a>(
        &'a self,
        requester: SessionIdentity,
        session_id: SessionId,
    ) -> Pin<Box<dyn Future<Output = Result<SessionTrace, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            session_id.validate()?;
            let session = self.authorized_session(&requester, &session_id).await?;
            let events = self.load(&session_id).await?;
            let runs = events
                .iter()
                .map(|event| event.run_id.clone())
                .collect::<BTreeSet<_>>();
            let scoped = self
                .state
                .snapshot()
                .await
                .sessions
                .into_iter()
                .filter(|candidate| {
                    candidate.identity.tenant_id == requester.tenant_id
                        && candidate.identity.user_id == requester.user_id
                })
                .collect::<Vec<_>>();
            let mut descendants = Vec::new();
            let mut seen = BTreeSet::new();
            let mut queue = VecDeque::from([session_id.clone()]);
            while let Some(parent) = queue.pop_front() {
                for child in scoped
                    .iter()
                    .filter(|candidate| candidate.parent_session_id.as_ref() == Some(&parent))
                {
                    if seen.insert(child.identity.session_id.clone()) {
                        descendants.push(child.identity.session_id.clone());
                        queue.push_back(child.identity.session_id.clone());
                    }
                }
            }
            Ok(SessionTrace {
                identity: session.identity,
                workspace_id: session.workspace_id,
                title: session.title,
                created_at_ms: session.created_at_ms,
                updated_at_ms: session.updated_at_ms,
                event_count: u64::try_from(events.len()).unwrap_or(u64::MAX),
                run_count: u64::try_from(runs.len()).unwrap_or(u64::MAX),
                first_seq: events.first().map(|event| event.seq),
                last_seq: events.last().map(|event| event.seq),
                parent_session_id: session.parent_session_id,
                descendant_session_ids: descendants,
            })
        })
    }
}

fn excerpt(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = normalized.chars();
    let prefix = characters.by_ref().take(240).collect::<String>();
    if characters.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}
