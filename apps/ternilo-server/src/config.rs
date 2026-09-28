use std::{
    fs,
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use clap::Args;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ternilo_control::SecretCipher;
use ternilo_protocol::HarnessError;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerConfig {
    pub version: u32,
    pub listen: SocketAddr,
    pub database_url: String,
    pub migration_database_url: Option<String>,
    pub secret_master_key: String,
    pub setup_token_hash: Option<String>,
    pub public_url: Option<String>,
    pub oidc: Option<OidcSettings>,
    pub max_database_connections: u32,
    pub managed_execution_enabled: bool,
    pub worker_policy: Option<PathBuf>,
    pub workspace_root: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OidcSettings {
    pub issuer: String,
    #[serde(default)]
    pub audience: String,
    pub client_id: String,
    pub scopes: String,
    pub allow_insecure: bool,
}

#[derive(Default, Args)]
pub(crate) struct ServeOptions {
    #[arg(long, env = "TERNILO_SERVER_CONFIG")]
    pub config: Option<PathBuf>,
    #[arg(long, env = "TERNILO_SERVER_LISTEN")]
    pub listen: Option<SocketAddr>,
    #[arg(long, env = "TERNILO_DATABASE_URL", hide_env_values = true)]
    pub database_url: Option<String>,
    #[arg(long, env = "TERNILO_MIGRATION_DATABASE_URL", hide_env_values = true)]
    pub migration_database_url: Option<String>,
    #[arg(long, env = "TERNILO_SECRET_MASTER_KEY", hide_env_values = true)]
    pub secret_master_key: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_SETUP_TOKEN", hide_env_values = true)]
    pub setup_token: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_PUBLIC_URL")]
    pub public_url: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_MAX_DATABASE_CONNECTIONS")]
    pub max_database_connections: Option<u32>,
    #[arg(long, env = "TERNILO_SERVER_OIDC_ISSUER")]
    pub oidc_issuer: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_OIDC_AUDIENCE")]
    pub oidc_audience: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_OIDC_CLIENT_ID")]
    pub oidc_client_id: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_OIDC_SCOPES")]
    pub oidc_scopes: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_ALLOW_INSECURE_OIDC", action = clap::ArgAction::Set, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    pub allow_insecure_oidc: Option<bool>,
    #[arg(long, env = "TERNILO_SERVER_MANAGED_EXECUTION_ENABLED", action = clap::ArgAction::Set, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    pub managed_execution_enabled: Option<bool>,
    #[arg(long, env = "TERNILO_SERVER_WORKER_POLICY")]
    pub worker_policy: Option<PathBuf>,
    #[arg(long, env = "TERNILO_SERVER_WORKSPACE_ROOT")]
    pub workspace_root: Option<PathBuf>,
}

impl ServerConfig {
    pub fn defaults(config_path: &Path, database_url: String, secret_master_key: String) -> Self {
        Self {
            version: 1,
            listen: SocketAddr::from(([0, 0, 0, 0], 4321)),
            database_url,
            migration_database_url: None,
            secret_master_key,
            setup_token_hash: None,
            public_url: None,
            oidc: None,
            max_database_connections: 16,
            managed_execution_enabled: false,
            worker_policy: None,
            workspace_root: config_path
                .parent()
                .unwrap_or(Path::new("."))
                .join("workspaces"),
        }
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.version != 1 {
            return Err(HarnessError::invalid(
                "unsupported server configuration version",
            ));
        }
        if self.max_database_connections == 0 {
            return Err(HarnessError::invalid(
                "database connection count must be positive",
            ));
        }
        if !self.database_url.starts_with("sqlite:")
            && !self.database_url.starts_with("postgres://")
            && !self.database_url.starts_with("postgresql://")
        {
            return Err(HarnessError::invalid(
                "database URL must use sqlite or postgres",
            ));
        }
        SecretCipher::from_base64(&self.secret_master_key)?;
        if let Some(url) = &self.public_url {
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| HarnessError::invalid("public URL is invalid"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || !matches!(parsed.path(), "" | "/")
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(HarnessError::invalid(
                    "public URL must be an HTTP or HTTPS origin without credentials, query, or fragment",
                ));
            }
        }
        if let Some(oidc) = &self.oidc
            && (self.public_url.is_none()
                || oidc.issuer.trim().is_empty()
                || oidc.client_id.trim().is_empty())
        {
            return Err(HarnessError::invalid(
                "OIDC requires issuer, audience, client ID, and public URL",
            ));
        }
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self, HarnessError> {
        let bytes = fs::read(path).map_err(|error| {
            HarnessError::execution(format!(
                "read server configuration {}: {error}",
                path.display()
            ))
        })?;
        let config: Self = serde_json::from_slice(&bytes).map_err(|error| {
            HarnessError::invalid(format!("parse server configuration: {error}"))
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn write_new(&self, path: &Path) -> Result<(), HarnessError> {
        self.validate()?;
        create_private_parent(path)?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|error| {
            HarnessError::execution(format!("encode server configuration: {error}"))
        })?;
        bytes.push(b'\n');
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(|error| {
            HarnessError::execution(format!(
                "create private configuration {}: {error}",
                path.display()
            ))
        })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| HarnessError::execution(format!("save server configuration: {error}")))
    }
}

impl ServeOptions {
    pub fn load(self) -> Result<ServerConfig, HarnessError> {
        let path = self.config.clone().map_or_else(default_config_path, Ok)?;
        let mut config = if path.exists() {
            ServerConfig::read(&path)?
        } else if self.config.is_none()
            && self.database_url.is_some()
            && self.secret_master_key.is_some()
        {
            ServerConfig::defaults(
                &path,
                self.database_url.clone().unwrap_or_default(),
                self.secret_master_key.clone().unwrap_or_default(),
            )
        } else {
            return Err(HarnessError::invalid(format!(
                "server configuration {} does not exist; run ternilo-server init first",
                path.display()
            )));
        };
        if let Some(value) = self.listen {
            config.listen = value;
        }
        if let Some(value) = self.database_url {
            config.database_url = value;
        }
        if let Some(value) = self.migration_database_url {
            config.migration_database_url = Some(value);
        }
        if let Some(value) = self.secret_master_key {
            config.secret_master_key = value;
        }
        if let Some(value) = self.setup_token {
            config.setup_token_hash = Some(digest_token(&value));
        }
        if let Some(value) = self.public_url {
            config.public_url = Some(value);
        }
        if let Some(value) = self.max_database_connections {
            config.max_database_connections = value;
        }
        if let Some(value) = self.managed_execution_enabled {
            config.managed_execution_enabled = value;
        }
        if let Some(value) = self.worker_policy {
            config.worker_policy = Some(value);
        }
        if let Some(value) = self.workspace_root {
            config.workspace_root = value;
        }
        if self.oidc_issuer.is_some()
            || self.oidc_audience.is_some()
            || self.oidc_client_id.is_some()
            || self.oidc_scopes.is_some()
            || self.allow_insecure_oidc.is_some()
        {
            let mut oidc = config.oidc.unwrap_or(OidcSettings {
                issuer: String::new(),
                audience: String::new(),
                client_id: String::new(),
                scopes: "openid profile email".to_owned(),
                allow_insecure: false,
            });
            if let Some(value) = self.oidc_issuer {
                oidc.issuer = value;
            }
            if let Some(value) = self.oidc_audience {
                oidc.audience = value;
            }
            if let Some(value) = self.oidc_client_id {
                oidc.client_id = value;
            }
            if let Some(value) = self.oidc_scopes {
                oidc.scopes = value;
            }
            if let Some(value) = self.allow_insecure_oidc {
                oidc.allow_insecure = value;
            }
            config.oidc = Some(oidc);
        }
        config.validate()?;
        Ok(config)
    }
}

pub(crate) fn default_config_path() -> Result<PathBuf, HarnessError> {
    if let Some(root) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(root).join("ternilo-server/server.json"));
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            HarnessError::invalid("set --config or XDG_DATA_HOME when HOME is unavailable")
        })?;
    Ok(PathBuf::from(home).join(".local/share/ternilo-server/server.json"))
}

pub(crate) fn create_private_parent(path: &Path) -> Result<(), HarnessError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent).map_err(|error| {
        HarnessError::execution(format!("create server configuration directory: {error}"))
    })
}

pub(crate) fn accepts_setup_token(expected: Option<&str>, token: &str) -> bool {
    !token.is_empty() && expected.is_some_and(|value| value == digest_token(token))
}

pub(crate) fn digest_token(token: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(token.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
            output
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[test]
    fn configuration_is_private_and_existing_configuration_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private/server.json");
        let mut config = ServerConfig::defaults(
            &path,
            "sqlite::memory:".to_owned(),
            STANDARD.encode([3; 32]),
        );
        config.setup_token_hash = Some(digest_token("test-setup-token"));
        config.write_new(&path).unwrap();
        assert!(config.write_new(&path).is_err());
        let loaded = ServerConfig::read(&path).unwrap();
        assert!(accepts_setup_token(
            loaded.setup_token_hash.as_deref(),
            "test-setup-token"
        ));
        assert!(!accepts_setup_token(
            loaded.setup_token_hash.as_deref(),
            "wrong-token"
        ));
        assert!(
            !fs::read_to_string(&path)
                .unwrap()
                .contains("test-setup-token")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn explicit_serve_options_override_saved_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("server.json");
        let config = ServerConfig::defaults(
            &path,
            "sqlite::memory:".to_owned(),
            STANDARD.encode([3; 32]),
        );
        config.write_new(&path).unwrap();
        let loaded = ServeOptions {
            config: Some(path),
            listen: Some("127.0.0.1:5432".parse().unwrap()),
            managed_execution_enabled: Some(true),
            ..ServeOptions::default()
        }
        .load()
        .unwrap();
        assert_eq!(loaded.listen.port(), 5432);
        assert!(loaded.managed_execution_enabled);
        assert!(loaded.oidc.is_none());
    }
}
