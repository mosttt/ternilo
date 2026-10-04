use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::Args;
use ternilo_control::{ControlStore, InstanceSettings, NativeRegistration, SecretCipher};
use ternilo_protocol::HarnessError;

use crate::config::{ServerConfig, configuration_path, create_private_parent};

#[derive(Default, Args)]
pub(crate) struct SetupOptions {
    /// Instance directory containing config.json and persistent data.
    #[arg(long, env = "TERNILO_SERVER_CONFIG_DIR")]
    pub config_dir: Option<PathBuf>,
    #[arg(long, env = "TERNILO_DATABASE_URL", hide_env_values = true)]
    pub database_url: Option<String>,
    #[arg(long, env = "TERNILO_MIGRATION_DATABASE_URL", hide_env_values = true)]
    pub migration_database_url: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_LISTEN")]
    pub listen: Option<std::net::SocketAddr>,
    #[arg(long, env = "TERNILO_SERVER_PUBLIC_URL")]
    pub public_url: Option<String>,
    #[arg(long, env = "TERNILO_SECRET_MASTER_KEY", hide_env_values = true)]
    pub secret_master_key: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_OWNER_USERNAME")]
    pub owner_username: Option<String>,
    #[arg(long, env = "TERNILO_SERVER_OWNER_EMAIL")]
    pub owner_email: Option<String>,
    /// First-time owner password; omit it to use the interactive hidden prompt.
    #[arg(long, env = "TERNILO_SERVER_OWNER_PASSWORD", hide_env_values = true)]
    pub owner_password: Option<String>,
    /// Skip terminal prompts; owner credentials are optional.
    #[arg(long, env = "TERNILO_SERVER_NON_INTERACTIVE", action = clap::ArgAction::Set, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
    pub non_interactive: bool,
}

pub(crate) struct SetupOutcome {
    pub config_path: PathBuf,
    pub instance: Option<InstanceSettings>,
    pub browser_url: String,
}

pub(crate) async fn execute(options: SetupOptions) -> Result<(), HarnessError> {
    let outcome = initialize(options).await?;
    println!("Server configuration: {}", outcome.config_path.display());
    println!("Start with: ternilo-server serve --config-dir <instance-directory>");
    if outcome.instance.is_some() {
        println!(
            "The owner, default tenant, and project are ready. Sign in at {}",
            outcome.browser_url
        );
    } else {
        println!(
            "Start serve, then enter the Initialization Key from its logs in the owner setup page."
        );
    }
    Ok(())
}

pub(crate) async fn initialize(mut options: SetupOptions) -> Result<SetupOutcome, HarnessError> {
    let config_path = configuration_path(options.config_dir.take().as_deref())?;
    if config_path.exists() {
        return Err(HarnessError::conflict(
            "server configuration already exists; initialization does not overwrite it",
        ));
    }
    let interactive = !options.non_interactive && io::stdin().is_terminal();
    if options.database_url.is_none() && interactive {
        let choice = prompt("Database [sqlite/postgres]", "sqlite")?;
        match choice.as_str() {
            "sqlite" => {}
            "postgres" => {
                options.database_url = Some(hidden_prompt("PostgreSQL URL (input hidden): ")?);
            }
            _ => return Err(HarnessError::invalid("choose sqlite or postgres")),
        }
    }
    create_private_parent(&config_path)?;
    let database_url = match options.database_url {
        Some(url) => url,
        None => default_database_url(&config_path)?,
    };
    if database_url.contains(":memory:") {
        return Err(HarnessError::invalid(
            "server initialization requires a persistent database",
        ));
    }
    let secret_master_key = options
        .secret_master_key
        .unwrap_or_else(|| STANDARD.encode(rand::random::<[u8; 32]>()));
    let mut config = ServerConfig::defaults(&config_path, database_url, secret_master_key);
    config.migration_database_url = options.migration_database_url;
    config.public_url = options.public_url;
    if let Some(listen) = options.listen {
        config.listen = listen;
    }
    config.validate()?;

    // Validate the selected database before publishing a usable configuration.
    let store = ControlStore::connect(
        &config.database_url,
        config.migration_database_url.as_deref(),
        SecretCipher::from_base64(&config.secret_master_key)?,
        config.max_database_connections,
    )
    .await?;
    store.database().health().await?;
    if store.instance_settings().await?.is_some() {
        return Err(HarnessError::conflict(
            "this database already has an owner; use its existing configuration",
        ));
    }
    let registration = owner_registration(
        options.owner_username,
        options.owner_email,
        options.owner_password,
        interactive,
    )?;
    if let Some(registration) = &registration {
        registration.validate()?;
    }
    config.write_new(&config_path)?;
    let browser_url = config.public_url.clone().unwrap_or_else(|| {
        let address = if config.listen.ip().is_unspecified() {
            std::net::SocketAddr::from(([127, 0, 0, 1], config.listen.port()))
        } else {
            config.listen
        };
        format!("http://{address}")
    });
    let instance = if let Some(registration) = registration {
        Some(
            store
                .initialize_owner(&registration, now_ms()?)
                .await?
                .session
                .instance,
        )
    } else {
        None
    };
    store.database().close().await;
    Ok(SetupOutcome {
        config_path,
        instance,
        browser_url,
    })
}

fn owner_registration(
    username: Option<String>,
    email: Option<String>,
    password: Option<String>,
    interactive: bool,
) -> Result<Option<NativeRegistration>, HarnessError> {
    match (username, email, password) {
        (Some(username), Some(email), Some(password)) => Ok(Some(NativeRegistration {
            email,
            username,
            password,
        })),
        (username, email, password) if interactive => {
            let username = username.map_or_else(|| prompt("Owner username", "owner"), Ok)?;
            let email = email.map_or_else(|| prompt("Owner email", ""), Ok)?;
            let password = if let Some(password) = password {
                password
            } else {
                let password = hidden_prompt("Owner password: ")?;
                let repeated = hidden_prompt("Repeat owner password: ")?;
                if password != repeated {
                    return Err(HarnessError::invalid("passwords do not match"));
                }
                password
            };
            Ok(Some(NativeRegistration {
                email,
                username,
                password,
            }))
        }
        (None, None, None) => Ok(None),
        _ => Err(HarnessError::invalid(
            "provide owner username, email and password, or use the interactive setup",
        )),
    }
}

pub(crate) fn default_database_url(config_path: &Path) -> Result<String, HarnessError> {
    let database_path = config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("data/db/server.sqlite3");
    create_private_parent(&database_path)?;
    println!("SQLite database: {}", database_path.display());
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&database_path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(HarnessError::execution(format!(
                "prepare SQLite database: {error}"
            )));
        }
    }
    let mut url = reqwest::Url::parse("sqlite:///").expect("static SQLite URL is valid");
    url.set_path(
        database_path
            .to_str()
            .ok_or_else(|| HarnessError::invalid("SQLite path must be UTF-8"))?,
    );
    url.query_pairs_mut().append_pair("mode", "rwc");
    Ok(url.to_string())
}

fn prompt(label: &str, default: &str) -> Result<String, HarnessError> {
    print!("{label} [{default}]: ");
    io::stdout()
        .flush()
        .map_err(|error| HarnessError::execution(format!("write setup prompt: {error}")))?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| HarnessError::execution(format!("read setup input: {error}")))?;
    let value = value.trim();
    Ok(if value.is_empty() {
        default.to_owned()
    } else {
        value.to_owned()
    })
}

fn hidden_prompt(label: &str) -> Result<String, HarnessError> {
    rpassword::prompt_password(label)
        .map_err(|error| HarnessError::execution(format!("read private setup input: {error}")))
}

pub(crate) fn now_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_setup_creates_private_configuration_and_stable_owner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("server/config.json");
        let outcome = initialize(SetupOptions {
            config_dir: Some(path.parent().unwrap().to_path_buf()),
            owner_username: Some("owner".to_owned()),
            owner_email: Some("owner@example.test".to_owned()),
            owner_password: Some("test-owner-password".to_owned()),
            non_interactive: true,
            ..SetupOptions::default()
        })
        .await
        .unwrap();
        assert!(outcome.instance.is_some());
        let config = ServerConfig::read(&path).unwrap();
        assert!(config.database_url.starts_with("sqlite:"));
        let store = ControlStore::connect(
            &config.database_url,
            None,
            SecretCipher::from_base64(&config.secret_master_key).unwrap(),
            1,
        )
        .await
        .unwrap();
        let login = store
            .login_native("owner", "test-owner-password", now_ms().unwrap())
            .await
            .unwrap();
        assert_eq!(Some(login.session.instance), outcome.instance);
        assert!(
            initialize(SetupOptions {
                config_dir: Some(path.parent().unwrap().to_path_buf()),
                non_interactive: true,
                ..SetupOptions::default()
            })
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn named_configurations_initialize_separate_databases_beside_an_older_database() {
        let directory = tempfile::tempdir().unwrap();
        let old_path = directory.path().join("server.sqlite3");
        let old_url = format!("sqlite:{}", old_path.display());
        let old = ternilo_storage::Database::connect(&old_url, 1)
            .await
            .unwrap();
        old.initialize(
            "control",
            7,
            "CREATE TABLE legacy_marker (value TEXT); INSERT INTO legacy_marker VALUES ('retained');",
            "",
        )
        .await
        .unwrap();
        old.close().await;
        let old_bytes = fs::read(&old_path).unwrap();
        let mut urls = Vec::new();
        for name in ["first", "second"] {
            let instance = directory.path().join(name);
            initialize(SetupOptions {
                config_dir: Some(instance.clone()),
                non_interactive: true,
                ..SetupOptions::default()
            })
            .await
            .unwrap();
            let config = ServerConfig::read(&instance.join("config.json")).unwrap();
            assert!(instance.join("data/db/server.sqlite3").is_file());
            urls.push(config.database_url);
        }
        assert_ne!(urls[0], urls[1]);
        assert_eq!(fs::read(&old_path).unwrap(), old_bytes);
    }

    #[tokio::test]
    async fn deferred_setup_saves_database_settings_without_an_owner_or_plaintext_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let outcome = initialize(SetupOptions {
            config_dir: Some(directory.path().to_path_buf()),
            non_interactive: true,
            ..SetupOptions::default()
        })
        .await
        .unwrap();
        assert!(outcome.instance.is_none());
        assert!(
            ServerConfig::read(&path)
                .unwrap()
                .setup_token_hash
                .is_none()
        );
    }
}
