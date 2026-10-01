use std::{io::Write, net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};
use ternilo_protocol::{HarnessError, RunLimits};

use super::ServeOptions;

#[derive(Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LocalServiceConfig {
    pub version: u32,
    #[serde(skip)]
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub profile_layers: Vec<PathBuf>,
    pub max_steps: u32,
    pub max_tool_calls: u32,
    pub gateway_url: Option<String>,
    pub token: Option<String>,
    pub node_id: String,
    pub no_local_web: bool,
    pub allow_insecure_gateway: bool,
}

impl Default for LocalServiceConfig {
    fn default() -> Self {
        Self {
            version: 1,
            data_dir: PathBuf::new(),
            listen: SocketAddr::from(([127, 0, 0, 1], 3210)),
            profile_layers: Vec::new(),
            max_steps: 0,
            max_tool_calls: 0,
            gateway_url: None,
            token: None,
            node_id: "home".into(),
            no_local_web: false,
            allow_insecure_gateway: false,
        }
    }
}

impl LocalServiceConfig {
    pub fn run_limits(&self) -> RunLimits {
        RunLimits {
            max_steps: self.max_steps,
            max_tool_calls: self.max_tool_calls,
        }
    }
    fn validate(&self) -> Result<(), HarnessError> {
        if !self.listen.ip().is_loopback() {
            return Err(HarnessError::invalid(
                "local listen address must be loopback",
            ));
        }
        if let Some(url) = &self.gateway_url {
            crate::node::validate_gateway_url(url, self.allow_insecure_gateway)?;
            if self
                .token
                .as_deref()
                .is_none_or(|token| token.is_empty() || token.chars().any(char::is_whitespace))
            {
                return Err(HarnessError::invalid(
                    "a non-empty token without whitespace is required with a gateway URL",
                ));
            }
            ternilo_transport::ExecutorId::new(&self.node_id).validate()?;
        } else if self.token.is_some() {
            return Err(HarnessError::invalid(
                "a gateway URL is required with a remote token",
            ));
        }
        Ok(())
    }

    fn write_new(&self, root: &std::path::Path) -> Result<(), HarnessError> {
        ternilo_local::prepare_data_dir(root)?;
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|_| HarnessError::execution("encode local configuration"))?;
        bytes.push(b'\n');
        let mut file = tempfile::NamedTempFile::new_in(root).map_err(|error| {
            HarnessError::execution(format!("create private local configuration: {error}"))
        })?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|error| {
                HarnessError::execution(format!("save local configuration: {error}"))
            })?;
        file.persist_noclobber(root.join("config.json"))
            .map_err(|error| {
                HarnessError::execution(format!("publish local configuration: {}", error.error))
            })?;
        Ok(())
    }
}

impl ServeOptions {
    pub(crate) async fn load(self) -> Result<LocalServiceConfig, HarnessError> {
        let root = self
            .data_dir
            .clone()
            .map_or_else(ternilo_local::default_data_dir, Ok)?;
        let root = std::path::absolute(root).map_err(|error| {
            HarnessError::execution(format!("resolve local data directory: {error}"))
        })?;
        let path = root.join("config.json");
        let existing = match tokio::fs::read(&path).await {
            Ok(bytes) => Some(
                serde_json::from_slice::<LocalServiceConfig>(&bytes).map_err(|_| {
                    HarnessError::invalid("local config.json must be valid version 1 JSON")
                })?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "read local config.json: {error}"
                )));
            }
        };
        let is_new = existing.is_none();
        let mut config = existing.unwrap_or_default();
        config.data_dir.clone_from(&root);
        if config.version != 1 {
            return Err(HarnessError::invalid(
                "unsupported local configuration version",
            ));
        }
        // Paths saved in config.json belong to the instance, not the launcher's working directory.
        config.profile_layers = config
            .profile_layers
            .into_iter()
            .map(|path| root.join(path))
            .collect();
        if let Some(value) = self.listen {
            config.listen = value;
        }
        if !self.profile_layers.is_empty() {
            config.profile_layers = self
                .profile_layers
                .into_iter()
                .map(std::path::absolute)
                .collect::<Result<_, _>>()
                .map_err(|error| {
                    HarnessError::execution(format!("resolve profile path: {error}"))
                })?;
        }
        if let Some(value) = self.max_steps {
            config.max_steps = value;
        }
        if let Some(value) = self.max_tool_calls {
            config.max_tool_calls = value;
        }
        if let Some(value) = self.gateway_url {
            config.gateway_url = Some(value);
        }
        if let Some(value) = self.token {
            config.token = Some(value);
        }
        if let Some(value) = self.node_id {
            config.node_id = value;
        }
        if let Some(value) = self.no_local_web {
            config.no_local_web = value;
        }
        if let Some(value) = self.allow_insecure_gateway {
            config.allow_insecure_gateway = value;
        }
        config.validate()?;
        if is_new {
            config.write_new(&root)?;
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[tokio::test]
    async fn root_config_reopens_and_explicit_defaults_override_saved_values() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().to_str().unwrap();
        let first = ServeOptions::parse_from([
            "ternilo",
            "--data-dir",
            root,
            "--listen",
            "127.0.0.1:4567",
            "--max-steps",
            "12",
            "--no-local-web",
        ]);
        first.load().await.unwrap();
        let saved = tokio::fs::read(temporary.path().join("config.json"))
            .await
            .unwrap();
        let config = ServeOptions::parse_from(["ternilo", "--data-dir", root])
            .load()
            .await
            .unwrap();
        assert_eq!(config.listen.port(), 4567);
        assert_eq!(config.max_steps, 12);
        assert!(config.no_local_web);
        let config = ServeOptions::parse_from([
            "ternilo",
            "--data-dir",
            root,
            "--listen",
            "127.0.0.1:3210",
            "--max-steps",
            "0",
            "--no-local-web=false",
        ])
        .load()
        .await
        .unwrap();
        assert_eq!(config.listen.port(), 3210);
        assert_eq!(config.max_steps, 0);
        assert!(!config.no_local_web);
        assert_eq!(
            tokio::fs::read(temporary.path().join("config.json"))
                .await
                .unwrap(),
            saved
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(temporary.path().join("config.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
