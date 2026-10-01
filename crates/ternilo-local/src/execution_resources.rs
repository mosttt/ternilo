use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};
use ternilo_kernel::{ExecutionResourceControl, ExecutionResourceRegistry};
use ternilo_protocol::{HarnessError, RunId};
use tokio::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResourceOwner {
    pub(crate) session_id: String,
    pub(crate) run_id: RunId,
}
struct State {
    owners: BTreeMap<String, ResourceOwner>,
    controls: BTreeMap<String, Arc<dyn ExecutionResourceControl>>,
}

pub(crate) struct ExecutionResources {
    path: PathBuf,
    state: Mutex<State>,
}
impl ExecutionResources {
    pub(crate) async fn open(data_dir: &Path) -> Result<Self, HarnessError> {
        let path = data_dir.join("runtime/execution-resources.json");
        let owners = match tokio::fs::read(&path).await {
            Ok(data) => serde_json::from_slice(&data).map_err(|error| {
                HarnessError::execution(format!("read execution resource ownership: {error}"))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "read execution resource ownership: {error}"
                )));
            }
        };
        Ok(Self {
            path,
            state: Mutex::new(State {
                owners,
                controls: BTreeMap::new(),
            }),
        })
    }
    pub(crate) fn for_session(
        self: &Arc<Self>,
        session_id: String,
    ) -> Arc<dyn ExecutionResourceRegistry> {
        Arc::new(SessionResources {
            registry: Arc::clone(self),
            session_id,
        })
    }
    pub(crate) async fn owners(&self) -> Vec<(String, ResourceOwner)> {
        self.state
            .lock()
            .await
            .owners
            .iter()
            .map(|(id, owner)| (id.clone(), owner.clone()))
            .collect()
    }
    pub(crate) async fn prune(&self) -> Result<(), HarnessError> {
        let mut state = self.state.lock().await;
        let completed = state
            .controls
            .iter()
            .filter(|(_, control)| control.is_finished())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        if completed.is_empty() {
            return Ok(());
        }
        let mut owners = state.owners.clone();
        for id in &completed {
            owners.remove(id);
        }
        self.save(&owners).await?;
        state.owners = owners;
        for id in completed {
            state.controls.remove(&id);
        }
        Ok(())
    }
    pub(crate) async fn stop(&self, id: &str) -> Result<(), HarnessError> {
        let state = self.state.lock().await;
        if !state.owners.contains_key(id) {
            return Ok(());
        }
        let control = state.controls.get(id).cloned().ok_or_else(||HarnessError::unavailable(
            "Node restarted before execution resource completion was recorded; its process state remains unknown"))?;
        drop(state);
        control.stop().await?;
        self.prune().await
    }

    pub(crate) async fn stop_live(&self, session_id: Option<&str>) -> Result<(), HarnessError> {
        let ids = {
            let state = self.state.lock().await;
            state
                .controls
                .keys()
                .filter(|id| {
                    session_id.is_none_or(|session_id| {
                        state
                            .owners
                            .get(*id)
                            .is_some_and(|owner| owner.session_id == session_id)
                    })
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut failures = Vec::new();
        for id in ids {
            if let Err(error) = self.stop(&id).await {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(HarnessError::execution(format!(
                "execution resources could not finish shutdown: {}",
                failures.join("; ")
            )))
        }
    }
    async fn save(&self, owners: &BTreeMap<String, ResourceOwner>) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec(owners).map_err(|error| {
            HarnessError::execution(format!("encode execution resources: {error}"))
        })?;
        crate::persistence::atomic_replace(&self.path, &bytes, true).await
    }
}
struct SessionResources {
    registry: Arc<ExecutionResources>,
    session_id: String,
}
impl ExecutionResourceRegistry for SessionResources {
    fn register<'a>(
        &'a self,
        run_id: RunId,
        resource_id: String,
        control: Arc<dyn ExecutionResourceControl>,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            run_id.validate()?;
            self.registry.prune().await?;
            let mut state = self.registry.state.lock().await;
            if state.owners.contains_key(&resource_id) {
                return Err(HarnessError::conflict(
                    "execution resource ID already exists",
                ));
            }
            let mut owners = state.owners.clone();
            owners.insert(
                resource_id.clone(),
                ResourceOwner {
                    session_id: self.session_id.clone(),
                    run_id,
                },
            );
            self.registry.save(&owners).await?;
            state.owners = owners;
            state.controls.insert(resource_id, control);
            Ok(())
        })
    }
}
