//! Owner-enabled client updates; the gateway cannot choose installation paths or commands.
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use ternilo_local::LocalApplication;
use ternilo_protocol::{ClientUpdateInfo, ClientUpdatePhase, ClientUpdateProgress, HarnessError};
use ternilo_update::{SignedRelease, current_target};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

mod apply;
mod cli;
mod config;
mod installation;
pub use apply::{apply, recover};
pub(crate) use cli::installation_guard;
pub use cli::{UpdateCommand, execute};
use config::{UpdateSettings, io_error, load_settings, write_private};

pub const DATA_EPOCH: u32 = 1;

use ternilo_transport::ClientUpdateRequest as UpdateRequest;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPlan {
    request: UpdateRequest,
    settings: UpdateSettings,
    data_dir: PathBuf,
    archive: PathBuf,
    service_id: String,
    backup: PathBuf,
    previous_version: String,
    arguments: Vec<String>,
}

pub(crate) struct RemoteUpdater {
    application: Arc<LocalApplication>,
    state: Mutex<UpdaterState>,
    endpoint: String,
    scope: tokio::sync::RwLock<Option<ternilo_transport::ExecutorScope>>,
    executor_id: String,
    outgoing:
        tokio::sync::RwLock<Option<tokio::sync::mpsc::Sender<ternilo_transport::ExecutorFrame>>>,
    client: reqwest::Client,
    enabled_at_start: bool,
}
struct UpdaterState {
    progress: Option<ClientUpdateProgress>,
    cancellation: Option<CancellationToken>,
}

impl RemoteUpdater {
    pub(crate) fn new(
        application: Arc<LocalApplication>,
        gateway: &str,
        node_id: &str,
        installation_registered: bool,
    ) -> Result<Arc<Self>, HarnessError> {
        let binding = crate::node::server_binding(gateway, node_id)?;
        let endpoint = format!(
            "{}/api/v1/executors/client-updates",
            binding.server_url.trim_end_matches('/')
        );
        let progress = read_progress(application.data_dir())?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_mins(15))
            .build()
            .map_err(|_| HarnessError::execution("create client update connection"))?;
        Ok(Arc::new(Self {
            application,
            state: Mutex::new(UpdaterState {
                progress,
                cancellation: None,
            }),
            endpoint,
            scope: tokio::sync::RwLock::new(None),
            executor_id: node_id.to_owned(),
            outgoing: tokio::sync::RwLock::new(None),
            client,
            enabled_at_start: installation_registered,
        }))
    }

    pub(crate) async fn info(&self) -> Result<ClientUpdateInfo, HarnessError> {
        let root = self.application.data_dir();
        let settings = load_settings(root)?;
        let connected = self
            .outgoing
            .read()
            .await
            .as_ref()
            .is_some_and(|sender| !sender.is_closed());
        {
            let mut state = self.state.lock().await;
            if let Some(progress) = read_progress(root)? {
                state.progress = Some(progress);
            }
            if let Some(progress) = &mut state.progress
                && connected
                && progress.phase == ClientUpdatePhase::AwaitingReconnect
                && progress.version == env!("CARGO_PKG_VERSION")
                && crate::service::discover(root)
                    .await?
                    .is_some_and(|service| service.info.version == progress.version)
            {
                progress.phase = ClientUpdatePhase::Completed;
                progress.revision += 1;
                write_private(&root.join("data/updates/progress.json"), progress)?;
            }
        }
        Ok(ClientUpdateInfo {
            protocol_version: 1,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            target: current_target().map(str::to_owned),
            enabled: settings.is_some() && config::enabled(root)? && self.enabled_at_start,
            data_epoch: DATA_EPOCH,
            restart_method: settings
                .as_ref()
                .map(|settings| settings.restart.label().to_owned()),
            progress: self.state.lock().await.progress.clone(),
        })
    }

    pub(crate) async fn connected(
        self: &Arc<Self>,
        outgoing: tokio::sync::mpsc::Sender<ternilo_transport::ExecutorFrame>,
        scope: &ternilo_transport::ExecutorScope,
    ) {
        *self.scope.write().await = Some(scope.clone());
        *self.outgoing.write().await = Some(outgoing);
        let progress = self.state.lock().await.progress.clone();
        if let Some(progress) = progress
            && !progress.phase.terminal()
            && self.state.lock().await.cancellation.is_none()
            && !matches!(
                progress.phase,
                ClientUpdatePhase::Installing | ClientUpdatePhase::AwaitingReconnect
            )
        {
            let _ = self
                .set_progress(
                    ClientUpdatePhase::Failed,
                    Some("client restarted before installation; retry the update".into()),
                )
                .await;
        }
        let _ = self.report().await;
        let updater = Arc::clone(self);
        tokio::spawn(async move {
            for _ in 0..120 {
                let awaiting =
                    updater
                        .state
                        .lock()
                        .await
                        .progress
                        .as_ref()
                        .is_some_and(|progress| {
                            progress.phase == ClientUpdatePhase::AwaitingReconnect
                                && progress.version == env!("CARGO_PKG_VERSION")
                        });
                if !awaiting {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = updater.report().await;
            }
        });
    }

    async fn report(&self) -> Result<(), HarnessError> {
        let outgoing = self.outgoing.read().await.clone();
        if let Some(outgoing) = outgoing {
            outgoing
                .send(ternilo_transport::ExecutorFrame::ClientUpdateState {
                    info: self.info().await?,
                })
                .await
                .map_err(|_| {
                    HarnessError::unavailable(
                        "Server connection closed before reporting the update",
                    )
                })?;
        }
        Ok(())
    }

    async fn set_progress(
        &self,
        phase: ClientUpdatePhase,
        error: Option<String>,
    ) -> Result<(), HarnessError> {
        {
            let mut state = self.state.lock().await;
            if phase == ClientUpdatePhase::Installing
                && state
                    .cancellation
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
            {
                return Err(HarnessError::cancelled(
                    "client update cancelled before installation",
                ));
            }
            let progress = state
                .progress
                .as_mut()
                .ok_or_else(|| HarnessError::conflict("no client update is active"))?;
            progress.phase = phase;
            progress.error = error;
            progress.revision += 1;
            write_private(
                &self
                    .application
                    .data_dir()
                    .join("data/updates/progress.json"),
                progress,
            )?;
        }
        let _ = self.report().await;
        Ok(())
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        request: UpdateRequest,
    ) -> Result<ClientUpdateInfo, HarnessError> {
        request.validate()?;
        validate_request_id(&request.id)?;
        let root = self.application.data_dir();
        if !self.enabled_at_start {
            return Err(HarnessError::policy(
                "restart this client once after enabling remote updates",
            ));
        }
        let settings = load_settings(root)?.ok_or_else(|| {
            HarnessError::policy("the computer owner must enable remote updates locally first")
        })?;
        if !config::enabled(root)? {
            return Err(HarnessError::policy(
                "remote updates are disabled by the computer owner",
            ));
        }
        if request.expires_at_ms <= now_ms() || request.download_token.is_empty() {
            return Err(HarnessError::policy(
                "client update authorization has expired",
            ));
        }
        let release = SignedRelease {
            manifest: request.manifest.clone(),
            signature: request.signature.clone(),
        }
        .verify()?;
        release.validate_upgrade(
            env!("CARGO_PKG_VERSION"),
            &request.server_version,
            DATA_EPOCH,
        )?;
        let target = current_target()
            .ok_or_else(|| HarnessError::invalid("this platform has no remote update package"))?;
        release.artifact(target)?;
        let executable = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(io_error)?;
        if executable.parent() != Some(settings.installation.bin()) {
            return Err(HarnessError::policy(
                "remote updates were enabled for a different client installation",
            ));
        }
        let cancellation = CancellationToken::new();
        {
            let mut state = self.state.lock().await;
            if let Some(previous) = &state.progress {
                if previous.id == request.id {
                    drop(state);
                    return self.info().await;
                }
                if !previous.phase.terminal() {
                    return Err(HarnessError::conflict(
                        "another client update is still active",
                    ));
                }
            }
            let progress = ClientUpdateProgress {
                id: request.id.clone(),
                version: release.manifest().version.clone(),
                phase: ClientUpdatePhase::Downloading,
                revision: 1,
                error: None,
            };
            write_private(&root.join("data/updates/progress.json"), &progress)?;
            state.progress = Some(progress);
            state.cancellation = Some(cancellation.clone());
        }
        let _ = self.report().await;
        let updater = Arc::clone(self);
        tokio::spawn(async move {
            let result = updater.prepare(request, settings, cancellation).await;
            if let Err(error) = result {
                let phase = if error.is_cancelled() {
                    ClientUpdatePhase::Cancelled
                } else {
                    ClientUpdatePhase::Failed
                };
                let _ = updater.set_progress(phase, Some(error.to_string())).await;
            }
            updater.state.lock().await.cancellation = None;
        });
        self.info().await
    }

    async fn prepare(
        &self,
        request: UpdateRequest,
        settings: UpdateSettings,
        cancellation: CancellationToken,
    ) -> Result<(), HarnessError> {
        let release = SignedRelease {
            manifest: request.manifest.clone(),
            signature: request.signature.clone(),
        }
        .verify()?;
        let target =
            current_target().ok_or_else(|| HarnessError::invalid("unsupported update target"))?;
        let artifact = release.artifact(target)?.clone();
        let root = self.application.data_dir();
        let cache = root.join("cache/client-updates").join(&request.id);
        let archive = cache.join(&artifact.filename);
        let scope =
            self.scope.read().await.clone().ok_or_else(|| {
                HarnessError::unavailable("computer is not connected to its Server")
            })?;
        let mut download_url = reqwest::Url::parse(&self.endpoint)
            .map_err(|_| HarnessError::invalid("invalid Server update endpoint"))?;
        download_url
            .path_segments_mut()
            .map_err(|()| HarnessError::invalid("invalid Server update origin"))?
            .extend([
                scope.tenant_id.as_str(),
                &self.executor_id,
                &request.id,
                "package",
            ]);
        let download = async {
            let response = self
                .client
                .get(download_url)
                .bearer_auth(&request.download_token)
                .send()
                .await
                .map_err(|_| {
                    HarnessError::unavailable("cannot download update package from Server")
                })?
                .error_for_status()
                .map_err(|_| {
                    HarnessError::unavailable(
                        "Server rejected the update download; retry the update",
                    )
                })?;
            ternilo_update::save_response(response, &artifact, &archive).await
        };
        tokio::select! { () = cancellation.cancelled() => return Err(HarnessError::cancelled("client update cancelled")), result = download => result? }
        self.set_progress(ClientUpdatePhase::WaitingIdle, None)
            .await?;
        let maintenance = loop {
            if cancellation.is_cancelled() {
                return Err(HarnessError::cancelled("client update cancelled"));
            }
            if request.expires_at_ms <= now_ms() {
                return Err(HarnessError::unavailable(
                    "client update expired while waiting for this computer to become idle",
                ));
            }
            if let Some(guard) = self.application.try_client_update_maintenance().await? {
                break guard;
            }
            tokio::select! { () = cancellation.cancelled() => return Err(HarnessError::cancelled("client update cancelled")), () = tokio::time::sleep(Duration::from_secs(1)) => {} }
        };
        self.set_progress(ClientUpdatePhase::Installing, None)
            .await?;
        let service_id = crate::service::discover(root)
            .await?
            .ok_or_else(|| {
                HarnessError::unavailable("local service stopped before update handoff")
            })?
            .info
            .service_id;
        let mut request = request;
        request.download_token.clear();
        let plan = ApplyPlan {
            backup: settings
                .installation
                .control_dir()
                .join(format!(".ternilo-backup-{}", request.id)),
            request,
            settings,
            data_dir: root.to_owned(),
            archive,
            service_id,
            previous_version: env!("CARGO_PKG_VERSION").to_owned(),
            arguments: std::env::args().skip(1).collect(),
        };
        let path = cache.join("apply.json");
        write_private(&path, &plan)?;
        let result = apply::run_helper(&plan, &path, &cache).await;
        drop(maintenance);
        result
    }

    pub(crate) async fn cancel(&self, id: &str) -> Result<ClientUpdateInfo, HarnessError> {
        {
            let state = self.state.lock().await;
            let progress = state
                .progress
                .as_ref()
                .filter(|progress| progress.id == id)
                .ok_or_else(|| HarnessError::invalid("client update does not exist"))?;
            if matches!(
                progress.phase,
                ClientUpdatePhase::Installing | ClientUpdatePhase::AwaitingReconnect
            ) {
                return Err(HarnessError::conflict(
                    "installation has started; wait for it to finish",
                ));
            }
            if let Some(cancellation) = &state.cancellation {
                cancellation.cancel();
            }
        }
        self.info().await
    }
}

fn read_progress(root: &Path) -> Result<Option<ClientUpdateProgress>, HarnessError> {
    match std::fs::read(root.join("data/updates/progress.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| HarnessError::invalid("invalid client update progress")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}
fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
fn validate_request_id(id: &str) -> Result<(), HarnessError> {
    if id.len() < 16
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(HarnessError::invalid("invalid client update identifier"));
    }
    Ok(())
}
