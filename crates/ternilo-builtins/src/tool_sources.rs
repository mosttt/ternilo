use std::{collections::BTreeSet, sync::Arc};

use ternilo_kernel::{DeferredToolSource, RunCancellation, ToolRegistration};
use ternilo_protocol::{HarnessError, SessionServiceSnapshot, SessionServiceStatus};

use super::{SourceEntry, ToolRegistry, ToolState};

impl ToolRegistry {
    fn source_entries(&self) -> Vec<(u64, Arc<SourceEntry>)> {
        self.state
            .lock()
            .expect("tool registry lock poisoned")
            .sources
            .iter()
            .map(|(id, entry)| (*id, Arc::clone(entry)))
            .collect()
    }

    fn source_entry(&self, name: &str) -> Result<(u64, Arc<SourceEntry>), HarnessError> {
        let state = self
            .state
            .lock()
            .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
        let id = *state
            .source_names
            .get(name)
            .ok_or_else(|| HarnessError::invalid(format!("unknown background service {name:?}")))?;
        Ok((id, Arc::clone(&state.sources[&id])))
    }

    pub(super) fn source_snapshots(&self) -> Vec<SessionServiceSnapshot> {
        self.source_entries()
            .into_iter()
            .map(|(_, entry)| entry.source.snapshot())
            .collect()
    }

    pub(super) fn add_source(
        &self,
        source: Arc<dyn DeferredToolSource>,
    ) -> Result<u64, HarnessError> {
        let snapshot = source.snapshot();
        if snapshot.id.trim().is_empty() {
            return Err(HarnessError::invalid(
                "background service ID must not be empty",
            ));
        }
        let initial = source.initial_tools();
        let mut state = self
            .state
            .lock()
            .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
        // IDs are retained separately so registration never calls plugin code under this lock.
        if state.source_names.contains_key(&snapshot.id) {
            return Err(HarnessError::composition(format!(
                "background service {:?} is already registered",
                snapshot.id
            )));
        }
        let id = Self::next_id(&mut state)?;
        let entry = Arc::new(SourceEntry {
            source,
            gate: tokio::sync::Mutex::new(()),
        });
        validate_source_tools(&state, &[], &initial)?;
        state.sources.insert(id, entry);
        state.source_names.insert(snapshot.id, id);
        publish_source_tools(&mut state, id, initial)?;
        Ok(id)
    }

    pub(super) async fn remove_source(&self, id: u64) -> Result<(), HarnessError> {
        let entry = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
            let entry = state
                .sources
                .remove(&id)
                .ok_or_else(|| HarnessError::invalid("background service is no longer mounted"))?;
            state.source_names.retain(|_, registered| *registered != id);
            remove_source_tools(&mut state, id);
            entry
        };
        // Shutdown can cancel preparation before waiting for its serial publication gate.
        let result = entry.source.shutdown().await;
        let _gate = entry.gate.lock().await;
        result
    }

    fn publish_source(
        &self,
        id: u64,
        entry: &Arc<SourceEntry>,
        tools: Vec<ToolRegistration>,
    ) -> Result<(), HarnessError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
        if !state
            .sources
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            return Err(HarnessError::cancelled(
                "background service was unmounted during preparation",
            ));
        }
        publish_source_tools(&mut state, id, tools)
    }

    async fn reject_publication(&self, id: u64, entry: &Arc<SourceEntry>) {
        let still_mounted = self
            .state
            .lock()
            .expect("tool registry lock poisoned")
            .sources
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, entry));
        if still_mounted {
            let _ = entry.source.stop().await;
        } else {
            let _ = entry.source.shutdown().await;
        }
    }

    pub(super) async fn prepare_sources(
        &self,
        cancellation: RunCancellation,
    ) -> Result<(), HarnessError> {
        let entries = self.source_entries();
        if entries.is_empty() {
            return cancellation.check();
        }
        let _lease = self
            .environment
            .acquire_workspace(cancellation.clone())
            .await?;
        for (id, entry) in entries {
            let _gate = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(HarnessError::cancelled("tool preparation was cancelled")),
                gate = entry.gate.lock() => gate,
            };
            let tools = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(HarnessError::cancelled("tool preparation was cancelled")),
                result = entry.source.prepare(cancellation.clone()) => result?,
            };
            if let Err(error) = self.publish_source(id, &entry, tools) {
                self.reject_publication(id, &entry).await;
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) async fn start_service(
        &self,
        name: &str,
        cancellation: RunCancellation,
    ) -> Result<SessionServiceSnapshot, HarnessError> {
        let (id, entry) = self.source_entry(name)?;
        cancellation.check()?;
        let available = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(HarnessError::cancelled("service startup was cancelled")),
            lease = self.environment.try_acquire_workspace() => lease?,
        };
        let _lease = available.ok_or_else(|| {
            HarnessError::conflict("the working directory is in use; start the background service after it becomes available")
        })?;
        let _gate = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(HarnessError::cancelled("service startup was cancelled")),
            gate = entry.gate.lock() => gate,
        };
        let tools = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(HarnessError::cancelled("service startup was cancelled")),
            result = entry.source.start(cancellation.clone()) => result?,
        };
        if let Err(error) = self.publish_source(id, &entry, tools) {
            self.reject_publication(id, &entry).await;
            return Err(error);
        }
        Ok(entry.source.snapshot())
    }

    pub(super) async fn stop_service(
        &self,
        name: &str,
    ) -> Result<SessionServiceSnapshot, HarnessError> {
        let (id, entry) = self.source_entry(name)?;
        entry.source.stop().await?;
        let _gate = entry.gate.lock().await;
        let snapshot = entry.source.snapshot();
        if matches!(
            snapshot.status,
            SessionServiceStatus::Stopped | SessionServiceStatus::Idle
        ) {
            self.publish_source(id, &entry, entry.source.initial_tools())?;
        }
        Ok(snapshot)
    }
}

fn validate_source_tools(
    state: &ToolState,
    replaced: &[u64],
    tools: &[ToolRegistration],
) -> Result<(), HarnessError> {
    let mut names = BTreeSet::new();
    for tool in tools {
        let name = &tool.spec.name;
        if name.trim().is_empty() || !names.insert(name) {
            return Err(HarnessError::composition(
                "background service returned empty or duplicate tool names",
            ));
        }
        if state
            .names
            .get(name)
            .is_some_and(|id| !replaced.contains(id))
        {
            return Err(HarnessError::composition(format!(
                "tool {name:?} is already registered"
            )));
        }
    }
    Ok(())
}

fn remove_source_tools(state: &mut ToolState, source: u64) {
    for id in state.source_tools.remove(&source).unwrap_or_default() {
        if let Some(tool) = state.tools.remove(&id) {
            state.names.remove(&tool.spec.name);
        }
    }
}

fn publish_source_tools(
    state: &mut ToolState,
    source: u64,
    tools: Vec<ToolRegistration>,
) -> Result<(), HarnessError> {
    validate_source_tools(
        state,
        state.source_tools.get(&source).map_or(&[], Vec::as_slice),
        &tools,
    )?;
    let count =
        u64::try_from(tools.len()).map_err(|_| HarnessError::execution("too many source tools"))?;
    state
        .next
        .checked_add(count)
        .ok_or_else(|| HarnessError::execution("tool registration ID exhausted"))?;
    remove_source_tools(state, source);
    let mut ids = Vec::with_capacity(tools.len());
    for tool in tools {
        let id = ToolRegistry::next_id(state)?;
        state.names.insert(tool.spec.name.clone(), id);
        state.tools.insert(id, tool);
        ids.push(id);
    }
    state.source_tools.insert(source, ids);
    Ok(())
}
