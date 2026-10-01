use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use ternilo_protocol::HarnessError;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SandboxMode {
    /// Isolate each run in Linux namespaces without child network access.
    #[default]
    Bubblewrap,
    /// Use Linux namespaces inside a dedicated Worker container.
    Container,
    /// Child-process isolation only; intended for explicit development use.
    Process,
}

#[derive(Default, Args)]
pub(crate) struct InitOptions {
    #[arg(long, env = "TERNILO_WORKER_CONFIG_DIR")]
    pub config_dir: Option<PathBuf>,
    #[arg(long, env = "TERNILO_WORKER_SERVER_URL")]
    pub server_url: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_WORKSPACE_ROOT")]
    pub workspace_root: Option<PathBuf>,
    #[arg(long, env = "TERNILO_WORKER_SANDBOX", value_enum)]
    pub sandbox: Option<SandboxMode>,
    #[arg(long, env = "TERNILO_WORKER_HEALTH_LISTEN")]
    pub health_listen: Option<SocketAddr>,
    #[arg(long, env = "TERNILO_WORKER_POLL_INTERVAL_MS")]
    pub poll_interval_ms: Option<u64>,
    #[arg(long, env = "TERNILO_WORKER_MAX_ACTIVE_RUNS")]
    pub max_active_runs: Option<u32>,
    #[arg(long, env = "TERNILO_WORKER_MAX_RESIDENT_RUNS")]
    pub max_resident_runs: Option<u32>,
    #[arg(long, env = "TERNILO_WORKER_OTLP_ENDPOINT")]
    pub telemetry_endpoint: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_OTLP_HEADERS", hide_env_values = true)]
    pub telemetry_headers: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_OTLP_SERVICE_NAME")]
    pub telemetry_service_name: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_OTLP_TIMEOUT_MS")]
    pub telemetry_timeout_ms: Option<u64>,
}

#[derive(Default, Args)]
pub(crate) struct ServeOptions {
    #[arg(long, env = "TERNILO_WORKER_CONFIG_DIR")]
    pub config_dir: Option<PathBuf>,
    #[arg(long, env = "TERNILO_WORKER_SERVER_URL")]
    pub server_url: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_WORKSPACE_ROOT")]
    pub workspace_root: Option<PathBuf>,
    #[arg(long, env = "TERNILO_WORKER_SANDBOX", value_enum)]
    pub sandbox: Option<SandboxMode>,
    #[arg(long, env = "TERNILO_WORKER_HEALTH_LISTEN")]
    pub health_listen: Option<SocketAddr>,
    #[arg(long, env = "TERNILO_WORKER_POLL_INTERVAL_MS")]
    pub poll_interval_ms: Option<u64>,
    #[arg(long, env = "TERNILO_WORKER_MAX_ACTIVE_RUNS")]
    pub max_active_runs: Option<u32>,
    #[arg(long, env = "TERNILO_WORKER_MAX_RESIDENT_RUNS")]
    pub max_resident_runs: Option<u32>,
    #[arg(long, env = "TERNILO_WORKER_OTLP_ENDPOINT")]
    pub telemetry_endpoint: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_OTLP_HEADERS", hide_env_values = true)]
    pub telemetry_headers: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_OTLP_SERVICE_NAME")]
    pub telemetry_service_name: Option<String>,
    #[arg(long, env = "TERNILO_WORKER_OTLP_TIMEOUT_MS")]
    pub telemetry_timeout_ms: Option<u64>,
}

#[derive(Clone)]
pub(crate) struct LoadedWorkerConfig {
    pub server_url: String,
    pub token: String,
    pub workspace_root: PathBuf,
    pub sandbox: SandboxMode,
    pub health_listen: SocketAddr,
    pub poll_interval: Duration,
    pub capacity: ternilo_cloud::WorkerCapacity,
    pub telemetry_endpoint: Option<String>,
    pub telemetry_headers: Option<String>,
    pub telemetry_service_name: String,
    pub telemetry_timeout: Duration,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerConfig {
    version: u32,
    server_url: String,
    token: String,
    workspace_root: PathBuf,
    sandbox: SandboxMode,
    health_listen: SocketAddr,
    poll_interval_ms: u64,
    #[serde(default)]
    capacity: ternilo_cloud::WorkerCapacity,
    telemetry_endpoint: Option<String>,
    telemetry_headers: Option<String>,
    telemetry_service_name: String,
    telemetry_timeout_ms: u64,
}

impl InitOptions {
    pub fn initialize(self) -> Result<PathBuf, HarnessError> {
        let options = ServeOptions {
            config_dir: self.config_dir,
            server_url: self.server_url,
            token: self.token,
            workspace_root: self.workspace_root,
            sandbox: self.sandbox,
            health_listen: self.health_listen,
            poll_interval_ms: self.poll_interval_ms,
            max_active_runs: self.max_active_runs,
            max_resident_runs: self.max_resident_runs,
            telemetry_endpoint: self.telemetry_endpoint,
            telemetry_headers: self.telemetry_headers,
            telemetry_service_name: self.telemetry_service_name,
            telemetry_timeout_ms: self.telemetry_timeout_ms,
        };
        let path = configuration_path(options.config_dir.as_deref())?;
        if path.exists() {
            return Err(HarnessError::conflict(
                "Worker configuration already exists; initialization does not overwrite it",
            ));
        }
        let parent = path.parent().expect("absolute configuration has a parent");
        let config = options.apply(WorkerConfig {
            version: 1,
            server_url: String::new(),
            token: String::new(),
            workspace_root: parent.join("data/workspaces"),
            sandbox: SandboxMode::default(),
            health_listen: SocketAddr::from(([127, 0, 0, 1], 5431)),
            poll_interval_ms: 500,
            capacity: ternilo_cloud::WorkerCapacity::default(),
            telemetry_endpoint: None,
            telemetry_headers: None,
            telemetry_service_name: "ternilo-cloud".to_owned(),
            telemetry_timeout_ms: 10_000,
        })?;
        create_private_directory(parent)?;
        let mut bytes = serde_json::to_vec_pretty(&config)
            .map_err(|_| HarnessError::execution("encode Worker configuration"))?;
        bytes.push(b'\n');
        // Publish a complete private file without replacing another initializer's result.
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
            HarnessError::execution(format!("create private Worker configuration: {error}"))
        })?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|error| {
                HarnessError::execution(format!("save Worker configuration: {error}"))
            })?;
        file.persist_noclobber(&path).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                HarnessError::conflict(
                    "Worker configuration already exists; initialization does not overwrite it",
                )
            } else {
                HarnessError::execution(format!("publish Worker configuration: {}", error.error))
            }
        })?;
        Ok(path)
    }
}

impl ServeOptions {
    pub fn load(self) -> Result<LoadedWorkerConfig, HarnessError> {
        let path = configuration_path(self.config_dir.as_deref())?;
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                HarnessError::invalid(
                    "Worker configuration does not exist; run ternilo-worker init first",
                )
            } else {
                HarnessError::execution(format!("read Worker configuration: {error}"))
            }
        })?;
        // Do not include deserialized values in errors: this file contains private tokens.
        let config = serde_json::from_slice(&bytes).map_err(|_| {
            HarnessError::invalid("Worker configuration is not valid version 1 JSON")
        })?;
        let config = self.apply(config)?;
        Ok(LoadedWorkerConfig {
            server_url: config.server_url,
            token: config.token,
            workspace_root: config.workspace_root,
            sandbox: config.sandbox,
            health_listen: config.health_listen,
            poll_interval: Duration::from_millis(config.poll_interval_ms),
            capacity: config.capacity,
            telemetry_endpoint: config.telemetry_endpoint,
            telemetry_headers: config.telemetry_headers,
            telemetry_service_name: config.telemetry_service_name,
            telemetry_timeout: Duration::from_millis(config.telemetry_timeout_ms),
        })
    }

    fn apply(self, mut config: WorkerConfig) -> Result<WorkerConfig, HarnessError> {
        if let Some(value) = self.server_url {
            config.server_url = value;
        }
        if let Some(value) = self.token {
            config.token = value;
        }
        if let Some(value) = self.workspace_root {
            config.workspace_root = value;
        }
        if let Some(value) = self.sandbox {
            config.sandbox = value;
        }
        if let Some(value) = self.health_listen {
            config.health_listen = value;
        }
        if let Some(value) = self.poll_interval_ms {
            config.poll_interval_ms = value;
        }
        if let Some(value) = self.telemetry_endpoint {
            config.telemetry_endpoint = Some(value);
        }
        if let Some(value) = self.telemetry_headers {
            config.telemetry_headers = Some(value);
        }
        if let Some(value) = self.telemetry_service_name {
            config.telemetry_service_name = value;
        }
        if let Some(value) = self.telemetry_timeout_ms {
            config.telemetry_timeout_ms = value;
        }
        if let Some(value) = self.max_active_runs {
            config.capacity.max_active_runs = value;
        }
        if let Some(value) = self.max_resident_runs {
            config.capacity.max_resident_runs = value;
        }
        config.validate()?;
        config.workspace_root = absolute_path(&config.workspace_root)?;
        config.server_url = config.server_url.trim_end_matches('/').to_owned();
        Ok(config)
    }
}

impl WorkerConfig {
    fn validate(&self) -> Result<(), HarnessError> {
        self.capacity.validate()?;
        if self.telemetry_timeout_ms > 1_800_000 {
            return Err(HarnessError::invalid(
                "Worker telemetry timeout must not exceed 30 minutes",
            ));
        }
        if self.version != 1 {
            return Err(HarnessError::invalid(
                "unsupported Worker configuration version",
            ));
        }
        validate_http_url(&self.server_url, true)?;
        if self.token.is_empty() || self.token.chars().any(char::is_whitespace) {
            return Err(HarnessError::invalid(
                "Worker token must be nonempty and contain no whitespace",
            ));
        }
        if self.workspace_root.as_os_str().is_empty() {
            return Err(HarnessError::invalid(
                "Worker workspace root must not be empty",
            ));
        }
        if self.poll_interval_ms == 0 || self.telemetry_timeout_ms == 0 {
            return Err(HarnessError::invalid(
                "Worker poll interval and telemetry timeout must be positive",
            ));
        }
        if self.telemetry_service_name.trim().is_empty() {
            return Err(HarnessError::invalid(
                "Worker telemetry service name must not be empty",
            ));
        }
        if let Some(endpoint) = &self.telemetry_endpoint {
            validate_http_url(endpoint, false)?;
        }
        if let Some(headers) = &self.telemetry_headers {
            serde_json::from_str::<BTreeMap<String, String>>(headers).map_err(|_| {
                HarnessError::invalid("Worker telemetry headers must be a JSON object of strings")
            })?;
        }
        Ok(())
    }
}

fn validate_http_url(value: &str, origin_only: bool) -> Result<(), HarnessError> {
    let invalid = || {
        HarnessError::invalid(if origin_only {
            "Worker Server URL must be an HTTP or HTTPS origin without credentials, query, or fragment"
        } else {
            "Worker telemetry endpoint must be an HTTP or HTTPS URL without credentials or fragment"
        })
    };
    let url = reqwest::Url::parse(value).map_err(|_| invalid())?;
    if value.trim() != value
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (origin_only && (!matches!(url.path(), "" | "/") || url.query().is_some()))
    {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn default_config_path() -> Result<PathBuf, HarnessError> {
    default_path_from_environment(
        std::env::var_os("XDG_DATA_HOME").as_deref().map(Path::new),
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

fn default_path_from_environment(
    xdg: Option<&Path>,
    home: Option<&Path>,
) -> Result<PathBuf, HarnessError> {
    if let Some(root) = xdg.filter(|path| !path.as_os_str().is_empty()) {
        return Ok(root.join("ternilo-worker/config.json"));
    }
    let home = home
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            HarnessError::invalid("set --config-dir or XDG_DATA_HOME when HOME is unavailable")
        })?;
    Ok(home.join(".local/share/ternilo-worker/config.json"))
}

fn configuration_path(directory: Option<&Path>) -> Result<PathBuf, HarnessError> {
    match directory {
        Some(directory) => Ok(absolute_path(directory)?.join("config.json")),
        None => default_config_path(),
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, HarnessError> {
    if path.as_os_str().is_empty() {
        return Err(HarnessError::invalid(
            "Worker configuration paths must not be empty",
        ));
    }
    std::path::absolute(path)
        .map_err(|error| HarnessError::execution(format!("resolve Worker path: {error}")))
}

fn create_private_directory(path: &Path) -> Result<(), HarnessError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        HarnessError::execution(format!("create Worker configuration directory: {error}"))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path).map_err(|error| {
            HarnessError::execution(format!("read Worker directory permissions: {error}"))
        })?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(HarnessError::invalid(
                "Worker configuration directory must be private (mode 0700)",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[derive(Parser)]
    struct InitCli {
        #[command(flatten)]
        options: InitOptions,
    }

    #[derive(Parser)]
    struct ServeCli {
        #[command(flatten)]
        options: ServeOptions,
    }

    fn fixture(directory: &Path) -> InitOptions {
        InitOptions {
            config_dir: Some(directory.to_path_buf()),
            server_url: Some("https://server.example/".to_owned()),
            token: Some("private-worker-token".to_owned()),
            ..InitOptions::default()
        }
    }

    #[test]
    fn initialization_is_private_and_never_replaces_an_existing_configuration() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("worker");
        let path = fixture(&directory).initialize().unwrap();
        let original = fs::read(&path).unwrap();
        assert!(fixture(&directory).initialize().is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(value["token"], "private-worker-token");
        for removed in [
            "database_url",
            "host_database_url",
            "secret_master_key",
            "policy",
            "lease_seconds",
        ] {
            assert!(value.get(removed).is_none());
        }
        let loaded = ServeOptions {
            config_dir: Some(path.parent().unwrap().to_path_buf()),
            ..ServeOptions::default()
        }
        .load()
        .unwrap();
        assert_eq!(loaded.server_url, "https://server.example");
        assert_eq!(loaded.workspace_root, directory.join("data/workspaces"));
        assert_eq!(loaded.sandbox, SandboxMode::Bubblewrap);
        assert_eq!(loaded.health_listen, "127.0.0.1:5431".parse().unwrap());
        assert_eq!(loaded.poll_interval, Duration::from_millis(500));
        assert_eq!(loaded.capacity.max_active_runs, 4);
        assert_eq!(loaded.capacity.max_resident_runs, 16);
        assert_eq!(value["capacity"]["max_active_runs"], 4);
        assert_eq!(value["capacity"]["max_resident_runs"], 16);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn runtime_overrides_do_not_rewrite_persisted_credentials_or_operator_settings() {
        let temporary = tempfile::tempdir().unwrap();
        let mut init = fixture(&temporary.path().join("worker"));
        init.max_active_runs = Some(6);
        init.max_resident_runs = Some(24);
        init.telemetry_endpoint = Some("https://collector.example/v1/logs".to_owned());
        init.telemetry_headers = Some(r#"{"authorization":"Bearer telemetry-secret"}"#.to_owned());
        let path = init.initialize().unwrap();
        let original = fs::read(&path).unwrap();
        let saved = ServeOptions {
            config_dir: Some(path.parent().unwrap().to_path_buf()),
            ..ServeOptions::default()
        }
        .load()
        .unwrap();
        assert_eq!(saved.capacity.max_active_runs, 6);
        assert_eq!(saved.capacity.max_resident_runs, 24);
        let loaded = ServeOptions {
            config_dir: Some(path.parent().unwrap().to_path_buf()),
            server_url: Some("http://127.0.0.1:4321".to_owned()),
            token: Some("replacement-token".to_owned()),
            sandbox: Some(SandboxMode::Process),
            workspace_root: Some(temporary.path().join("other-workspaces")),
            health_listen: Some("127.0.0.1:0".parse().unwrap()),
            poll_interval_ms: Some(25),
            max_active_runs: Some(8),
            max_resident_runs: Some(32),
            telemetry_service_name: Some("worker-test".to_owned()),
            telemetry_timeout_ms: Some(1000),
            ..ServeOptions::default()
        }
        .load()
        .unwrap();
        assert_eq!(loaded.token, "replacement-token");
        assert_eq!(loaded.server_url, "http://127.0.0.1:4321");
        assert_eq!(loaded.sandbox, SandboxMode::Process);
        assert_eq!(loaded.poll_interval, Duration::from_millis(25));
        assert_eq!(loaded.capacity.max_active_runs, 8);
        assert_eq!(loaded.capacity.max_resident_runs, 32);
        assert_eq!(
            loaded.telemetry_endpoint.as_deref(),
            Some("https://collector.example/v1/logs")
        );
        assert!(
            loaded
                .telemetry_headers
                .unwrap()
                .contains("telemetry-secret")
        );
        assert_eq!(loaded.telemetry_service_name, "worker-test");
        assert_eq!(loaded.telemetry_timeout, Duration::from_secs(1));
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn blank_or_invalid_overrides_are_rejected_without_leaking_private_values() {
        let temporary = tempfile::tempdir().unwrap();
        let path = fixture(&temporary.path().join("worker"))
            .initialize()
            .unwrap();
        let overrides = [
            ServeOptions {
                token: Some(String::new()),
                ..ServeOptions::default()
            },
            ServeOptions {
                token: Some("private worker token".to_owned()),
                ..ServeOptions::default()
            },
            ServeOptions {
                server_url: Some(String::new()),
                ..ServeOptions::default()
            },
            ServeOptions {
                server_url: Some("https://user:secret@server.example".to_owned()),
                ..ServeOptions::default()
            },
            ServeOptions {
                server_url: Some("https://server.example/api?token=secret".to_owned()),
                ..ServeOptions::default()
            },
            ServeOptions {
                workspace_root: Some(PathBuf::new()),
                ..ServeOptions::default()
            },
            ServeOptions {
                poll_interval_ms: Some(0),
                ..ServeOptions::default()
            },
            ServeOptions {
                telemetry_endpoint: Some(String::new()),
                ..ServeOptions::default()
            },
            ServeOptions {
                telemetry_headers: Some("private-invalid-json-secret".to_owned()),
                ..ServeOptions::default()
            },
            ServeOptions {
                telemetry_service_name: Some(" ".to_owned()),
                ..ServeOptions::default()
            },
            ServeOptions {
                telemetry_timeout_ms: Some(0),
                ..ServeOptions::default()
            },
        ];
        for mut options in overrides {
            options.config_dir = Some(path.parent().unwrap().to_path_buf());
            let error = options.load().err().expect("invalid override must fail");
            assert!(!error.message.contains("secret"));
            assert!(!error.message.contains("private worker token"));
        }
        assert!(
            ServeOptions {
                config_dir: Some(PathBuf::new()),
                ..ServeOptions::default()
            }
            .load()
            .is_err()
        );
    }

    #[test]
    fn capacity_limits_reject_zero_and_resident_capacity_below_active_capacity() {
        let temporary = tempfile::tempdir().unwrap();
        let path = fixture(&temporary.path().join("saved"))
            .initialize()
            .unwrap();
        let original = fs::read(&path).unwrap();
        for (active, resident) in [(0, 16), (4, 0), (5, 4)] {
            let directory = temporary
                .path()
                .join(format!("invalid-{active}-{resident}"));
            let mut init = fixture(&directory);
            init.max_active_runs = Some(active);
            init.max_resident_runs = Some(resident);
            assert!(init.initialize().is_err());
            assert!(
                !directory.exists(),
                "invalid initialization must not publish configuration"
            );
            assert!(
                ServeOptions {
                    config_dir: Some(path.parent().unwrap().to_path_buf()),
                    max_active_runs: Some(active),
                    max_resident_runs: Some(resident),
                    ..ServeOptions::default()
                }
                .load()
                .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn initialization_requires_a_server_and_token_before_creating_files() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("uninitialized");
        for (server_url, token) in [
            (None, Some("token".to_owned())),
            (Some("https://server.example".to_owned()), None),
        ] {
            assert!(
                InitOptions {
                    config_dir: Some(directory.clone()),
                    server_url,
                    token,
                    ..InitOptions::default()
                }
                .initialize()
                .is_err()
            );
            assert!(!directory.exists());
        }
        assert!(
            ServeOptions {
                config_dir: Some(directory),
                ..ServeOptions::default()
            }
            .load()
            .is_err()
        );
    }

    #[test]
    fn default_location_respects_xdg_and_home_without_mutating_process_environment() {
        assert_eq!(
            default_path_from_environment(
                Some(Path::new("/xdg")),
                Some(Path::new("/home/example"))
            )
            .unwrap(),
            Path::new("/xdg/ternilo-worker/config.json")
        );
        assert_eq!(
            default_path_from_environment(Some(Path::new("")), Some(Path::new("/home/example")))
                .unwrap(),
            Path::new("/home/example/.local/share/ternilo-worker/config.json")
        );
        assert!(default_path_from_environment(None, None).is_err());
    }

    #[test]
    fn configuration_rejects_legacy_database_and_policy_fields() {
        let temporary = tempfile::tempdir().unwrap();
        let path = fixture(&temporary.path().join("worker"))
            .initialize()
            .unwrap();
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for key in [
            "database_url",
            "host_database_url",
            "secret_master_key",
            "policy",
        ] {
            let mut value = original.clone();
            value[key] = "private-legacy-secret".into();
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let error = ServeOptions {
                config_dir: Some(path.parent().unwrap().to_path_buf()),
                ..ServeOptions::default()
            }
            .load()
            .err()
            .unwrap();
            assert!(!error.message.contains("private-legacy-secret"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn initialization_does_not_change_permissions_on_an_existing_shared_directory() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let shared = temporary.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(fixture(&shared).initialize().is_err());
        assert!(!shared.join("config.json").exists());
        assert_eq!(
            fs::metadata(shared).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn cli_overrides_environment_and_environment_overrides_saved_configuration() {
        const CHILD: &str = "TERNILO_WORKER_CONFIG_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let cli = ServeCli::try_parse_from([
                "worker",
                "--server-url",
                "https://cli.example",
                "--token",
                "cli-token",
                "--max-active-runs",
                "5",
            ])
            .unwrap();
            let loaded = cli.options.load().unwrap();
            assert_eq!(loaded.server_url, "https://cli.example");
            assert_eq!(loaded.token, "cli-token");
            assert_eq!(loaded.sandbox, SandboxMode::Container);
            assert_eq!(loaded.poll_interval, Duration::from_millis(37));
            assert_eq!(loaded.capacity.max_active_runs, 5);
            assert_eq!(loaded.capacity.max_resident_runs, 12);
            assert_eq!(loaded.health_listen, "127.0.0.1:5439".parse().unwrap());
            assert_eq!(loaded.telemetry_service_name, "environment-worker");
            assert_eq!(
                loaded.telemetry_endpoint.as_deref(),
                Some("https://collector.example/v1/logs")
            );
            assert_eq!(
                loaded.telemetry_headers.as_deref(),
                Some(r#"{"authorization":"Bearer environment-secret"}"#)
            );
            assert_eq!(loaded.telemetry_timeout, Duration::from_millis(4321));
            let init = InitCli::try_parse_from([
                "worker",
                "--token",
                "init-cli-token",
                "--max-active-runs",
                "6",
            ])
            .unwrap();
            assert_eq!(init.options.max_active_runs, Some(6));
            assert_eq!(init.options.max_resident_runs, Some(12));
            assert_eq!(init.options.token.as_deref(), Some("init-cli-token"));
            assert_eq!(
                init.options.server_url.as_deref(),
                Some("https://environment.example")
            );
            return;
        }
        let temporary = tempfile::tempdir().unwrap();
        let path = fixture(&temporary.path().join("worker"))
            .initialize()
            .unwrap();
        let original = fs::read(&path).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.env_clear();
        if let Some(search_path) = std::env::var_os("LD_LIBRARY_PATH") {
            child.env("LD_LIBRARY_PATH", search_path);
        }
        let status = child
            .args(["--exact", "config::tests::cli_overrides_environment_and_environment_overrides_saved_configuration"])
            .env(CHILD, "1")
            .env("TERNILO_WORKER_CONFIG_DIR", path.parent().unwrap())
            .env("TERNILO_WORKER_SERVER_URL", "https://environment.example")
            .env("TERNILO_WORKER_TOKEN", "environment-token")
            .env("TERNILO_WORKER_SANDBOX", "container")
            .env("TERNILO_WORKER_POLL_INTERVAL_MS", "37")
            .env("TERNILO_WORKER_MAX_ACTIVE_RUNS", "3")
            .env("TERNILO_WORKER_MAX_RESIDENT_RUNS", "12")
            .env("TERNILO_WORKER_HEALTH_LISTEN", "127.0.0.1:5439")
            .env("TERNILO_WORKER_OTLP_ENDPOINT", "https://collector.example/v1/logs")
            .env("TERNILO_WORKER_OTLP_HEADERS", r#"{"authorization":"Bearer environment-secret"}"#)
            .env("TERNILO_WORKER_OTLP_SERVICE_NAME", "environment-worker")
            .env("TERNILO_WORKER_OTLP_TIMEOUT_MS", "4321")
            .status().unwrap();
        assert!(status.success());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn cli_arguments_keep_worker_environment_names_and_hide_secrets() {
        for command in [InitCli::command(), ServeCli::command()] {
            command.clone().debug_assert();
            for (id, environment) in [
                ("server_url", "TERNILO_WORKER_SERVER_URL"),
                ("token", "TERNILO_WORKER_TOKEN"),
                ("config_dir", "TERNILO_WORKER_CONFIG_DIR"),
                ("workspace_root", "TERNILO_WORKER_WORKSPACE_ROOT"),
                ("sandbox", "TERNILO_WORKER_SANDBOX"),
                ("health_listen", "TERNILO_WORKER_HEALTH_LISTEN"),
                ("poll_interval_ms", "TERNILO_WORKER_POLL_INTERVAL_MS"),
                ("max_active_runs", "TERNILO_WORKER_MAX_ACTIVE_RUNS"),
                ("max_resident_runs", "TERNILO_WORKER_MAX_RESIDENT_RUNS"),
                ("telemetry_endpoint", "TERNILO_WORKER_OTLP_ENDPOINT"),
                ("telemetry_headers", "TERNILO_WORKER_OTLP_HEADERS"),
                ("telemetry_service_name", "TERNILO_WORKER_OTLP_SERVICE_NAME"),
                ("telemetry_timeout_ms", "TERNILO_WORKER_OTLP_TIMEOUT_MS"),
            ] {
                let argument = command
                    .get_arguments()
                    .find(|argument| argument.get_id() == id)
                    .unwrap();
                assert_eq!(argument.get_env().unwrap(), environment);
                if ["token", "telemetry_headers"].contains(&id) {
                    assert!(argument.is_hide_env_values_set());
                }
            }
            assert!(command.get_arguments().all(|argument| {
                ![
                    "database_url",
                    "host_database_url",
                    "secret_master_key",
                    "policy",
                ]
                .contains(&argument.get_id().as_str())
            }));
        }
    }
}
