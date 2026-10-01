//! Local service lifecycle and authenticated discovery shared by all local clients.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use clap::Parser;
use serde::{Deserialize, Serialize};
use ternilo_protocol::{HarnessError, RunLimits};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};

mod browser;
mod config;
pub(crate) use config::LocalServiceConfig;

const DISCOVERY_FILE: &str = "runtime/service.json";

#[derive(Clone, Debug, Parser)]
pub struct ServeOptions {
    /// Loopback address for the local browser and management API.
    #[arg(long, env = "TERNILO_LOCAL_LISTEN")]
    pub listen: Option<SocketAddr>,
    /// Additional profile layers applied over the local default.
    #[arg(
        long = "profile",
        env = "TERNILO_LOCAL_PROFILES",
        value_delimiter = ','
    )]
    pub profile_layers: Vec<PathBuf>,
    #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Host ceiling for Agent steps per turn; 0 means no host ceiling.
    #[arg(long, env = "TERNILO_LOCAL_MAX_STEPS")]
    pub max_steps: Option<u32>,
    /// Host ceiling for tool calls per turn; 0 leaves the Agent configuration unrestricted.
    #[arg(long, env = "TERNILO_LOCAL_MAX_TOOL_CALLS")]
    pub max_tool_calls: Option<u32>,
    /// Connect this computer to the Ternilo Server WebSocket gateway.
    #[arg(long, env = "TERNILO_LOCAL_GATEWAY_URL")]
    pub gateway_url: Option<String>,
    /// This computer's Server credential. Prefer the environment to command-line secrets.
    #[arg(long, env = "TERNILO_LOCAL_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
    #[arg(long, env = "TERNILO_LOCAL_NODE_ID")]
    pub node_id: Option<String>,
    /// Disable the local browser UI while keeping the management API available.
    #[arg(long, env = "TERNILO_LOCAL_NO_LOCAL_WEB", action = clap::ArgAction::Set,
        num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    pub no_local_web: Option<bool>,
    /// Open the local web UI in the default browser once the service is ready.
    #[arg(long, alias = "open-brower")]
    pub open_browser: bool,
    /// Permit plaintext ws:// for development and tests.
    #[arg(long, env = "TERNILO_LOCAL_ALLOW_INSECURE_GATEWAY", action = clap::ArgAction::Set,
        num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    pub allow_insecure_gateway: Option<bool>,
}

impl ServeOptions {
    #[must_use]
    pub fn run_limits(&self) -> RunLimits {
        RunLimits {
            max_steps: self.max_steps.unwrap_or(0),
            max_tool_calls: self.max_tool_calls.unwrap_or(0),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServiceInfo {
    pub service_id: String,
    pub version: String,
    pub pid: u32,
    pub address: SocketAddr,
    pub browser_enabled: bool,
    pub remote_enabled: bool,
}

impl ServiceInfo {
    #[must_use]
    pub fn origin(&self) -> String {
        format!("http://{}", self.address)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ServiceConnection {
    pub info: ServiceInfo,
    pub api_token: String,
}

#[derive(Clone)]
pub(crate) struct ServiceControl {
    pub info: ServiceInfo,
    pub shutdown: Arc<Notify>,
    pub http_shutdown: CancellationToken,
}

struct Registration(PathBuf);

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn client() -> Result<reqwest::Client, HarnessError> {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| HarnessError::execution(format!("create local service client: {error}")))
}

/// Discover and authenticate a service belonging to this data directory.
pub async fn discover(data_dir: &Path) -> Result<Option<ServiceConnection>, HarnessError> {
    let bytes = match fs::read(data_dir.join(DISCOVERY_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HarnessError::execution(format!(
                "read local service information: {error}"
            )));
        }
    };
    let connection: ServiceConnection = serde_json::from_slice(&bytes).map_err(|error| {
        HarnessError::invalid(format!("invalid local service information: {error}"))
    })?;
    if !connection.info.address.ip().is_loopback() {
        return Err(HarnessError::policy(
            "local service discovery requires a loopback address",
        ));
    }
    let response = match client()?
        .get(format!("{}/api/v1/service", connection.info.origin()))
        .bearer_auth(&connection.api_token)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) if error.is_connect() || error.is_timeout() => return Ok(None),
        Err(error) => {
            return Err(HarnessError::execution(format!(
                "connect to local service: {error}"
            )));
        }
    };
    let info: ServiceInfo = response
        .error_for_status()
        .map_err(|error| {
            HarnessError::policy(format!("local service authentication failed: {error}"))
        })?
        .json()
        .await
        .map_err(|error| {
            HarnessError::execution(format!("read local service identity: {error}"))
        })?;
    if info != connection.info {
        return Err(HarnessError::policy(
            "local service identity differs from its discovery record",
        ));
    }
    Ok(Some(connection))
}

pub async fn stop(data_dir: &Path) -> Result<(), HarnessError> {
    let Some(connection) = discover(data_dir).await? else {
        return Err(HarnessError::invalid(
            "no local service is running for this data directory",
        ));
    };
    client()?
        .post(format!("{}/api/v1/service/stop", connection.info.origin()))
        .bearer_auth(&connection.api_token)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| HarnessError::execution(format!("stop local service: {error}")))?;
    println!(
        "Stopping Ternilo local service (PID {})",
        connection.info.pid
    );
    for _ in 0..600 {
        match fs::read(data_dir.join(DISCOVERY_FILE)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(bytes) => {
                let current: ServiceConnection =
                    serde_json::from_slice(&bytes).map_err(|error| {
                        HarnessError::execution(format!("read service shutdown state: {error}"))
                    })?;
                if current.info.service_id != connection.info.service_id {
                    return Ok(());
                }
            }
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "read service shutdown state: {error}"
                )));
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(HarnessError::execution(
        "local service shutdown is still in progress; inspect its logs before restarting",
    ))
}

pub async fn run(options: ServeOptions) -> Result<(), HarnessError> {
    let open_browser = options.open_browser;
    let options = options.load().await?;
    if open_browser && options.no_local_web {
        return Err(HarnessError::invalid(
            "--open-browser requires the local web UI; use --no-local-web=false to enable it",
        ));
    }
    let data_dir = options.data_dir.clone();
    if let Some(connection) = discover(&data_dir).await? {
        println!("Ternilo local web: {}", connection.info.origin());
        return Err(HarnessError::invalid(
            "this local service is already running; use it or run ternilo stop before changing startup options",
        ));
    }
    let listener = crate::web::bind_loopback(options.listen).await?;
    let profile = crate::load_local_profile(&options.profile_layers)?;
    let server_binding = options
        .gateway_url
        .as_deref()
        .map(|url| crate::node::server_binding(url, &options.node_id))
        .transpose()?;
    let application = crate::open_local_application_with_options(
        profile,
        data_dir.clone(),
        options.run_limits(),
        ternilo_local::LocalApplicationOpenOptions { server_binding },
    )
    .await?;
    let mut registration = None;
    let commands = TaskTracker::new();
    let result = Box::pin(async {
        let address = listener
            .local_addr()
            .map_err(|error| HarnessError::execution(error.to_string()))?;
        let connection = ServiceConnection {
            info: ServiceInfo {
                service_id: crate::web::random_api_token(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                pid: std::process::id(),
                address,
                browser_enabled: !options.no_local_web,
                remote_enabled: options.gateway_url.is_some(),
            },
            api_token: crate::web::random_api_token(),
        };
        let shutdown = Arc::new(Notify::new());
        let http_shutdown = CancellationToken::new();
        let control = ServiceControl {
            info: connection.info.clone(),
            shutdown: Arc::clone(&shutdown),
            http_shutdown: http_shutdown.clone(),
        };
        registration = Some(register(&data_dir, &connection)?);
        let web = crate::web::serve_managed(
            listener,
            Arc::clone(&application),
            connection.api_token.clone(),
            Some(control),
        );
        let _browser = open_browser
            .then(|| AbortOnDropHandle::new(tokio::spawn(browser::open_when_ready(connection))));
        let gateway = async {
            if options.gateway_url.is_some() {
                Box::pin(crate::node::connect(
                    Arc::clone(&application),
                    &options,
                    commands.clone(),
                ))
                .await
            } else {
                std::future::pending::<Result<(), HarnessError>>().await
            }
        };
        tokio::pin!(web);
        let result = tokio::select! {
            result = &mut web => return result,
            result = gateway => result,
            () = shutdown.notified() => Ok(()),
            () = crate::web::shutdown_signal() => Ok(()),
        };
        http_shutdown.cancel();
        let web_result = web.await;
        result.and(web_result)
    })
    .await;
    commands.close();
    let shutdown = application.shutdown().await;
    commands.wait().await;
    let closed = application.close().await;
    drop(application);
    drop(registration);
    result?;
    shutdown.and(closed)
}

fn register(data_dir: &Path, connection: &ServiceConnection) -> Result<Registration, HarnessError> {
    let path = data_dir.join(DISCOVERY_FILE);
    let temporary = data_dir.join("runtime/.service.json.tmp");
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        // The application writer lock already owns this directory exclusively.
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(connection)?)?;
        file.sync_all()?;
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(&path)?;
        }
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    result.map_err(|error| HarnessError::execution(format!("register local service: {error}")))?;
    Ok(Registration(path))
}

/// Serialize desktop service starts without holding the application writer lock.
pub async fn start_lock(data_dir: &Path) -> Result<File, HarnessError> {
    ternilo_local::prepare_data_dir(data_dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join("runtime/service-start.lock"))
        .map_err(|error| HarnessError::execution(format!("open service startup lock: {error}")))?;
    for _ in 0..300 {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "lock service startup: {error}"
                )));
            }
        }
    }
    Err(HarnessError::execution(
        "another client is still starting the local service; try again",
    ))
}
