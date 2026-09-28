#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use ternilo_control::{ControlStore, SecretCipher};
use ternilo_protocol::HarnessError;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(about = "Ternilo Server backup and administration", version)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    ResetAuthentication {
        #[arg(long, env = "TERNILO_SERVER_CONFIG")]
        config: Option<std::path::PathBuf>,
    },
    /// Write a consistent SQLite database snapshot while Server remains online.
    BackupSqlite {
        #[arg(long, env = "TERNILO_SERVER_CONFIG")]
        config: Option<std::path::PathBuf>,
        #[arg(long, env = "TERNILO_DATABASE_URL", hide_env_values = true)]
        database_url: Option<String>,
        #[arg(long, env = "TERNILO_SERVER_BACKUP_OUTPUT")]
        output: std::path::PathBuf,
    },
    /// Transactionally re-encrypt all tenant secrets under a new master key.
    RotateSecretMasterKey {
        /// Server database URL (migration-owner account for PostgreSQL).
        #[arg(long, env = "TERNILO_MIGRATION_DATABASE_URL", hide_env_values = true)]
        database_url: String,
        /// Current base64-encoded master key.
        #[arg(long, env = "TERNILO_SECRET_MASTER_KEY", hide_env_values = true)]
        current_key: String,
        /// Next base64-encoded master key.
        #[arg(long, env = "TERNILO_NEXT_SECRET_MASTER_KEY", hide_env_values = true)]
        next_key: String,
    },
}

pub(crate) async fn execute(args: Args) -> Result<(), HarnessError> {
    match args.command {
        Command::ResetAuthentication { config } => {
            let path = config.map_or_else(crate::config::default_config_path, Ok)?;
            let config = crate::config::ServerConfig::read(&path)?;
            let store = ControlStore::connect(
                &config.database_url,
                config.migration_database_url.as_deref(),
                SecretCipher::from_base64(&config.secret_master_key)?,
                config.max_database_connections,
            )
            .await?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| HarnessError::execution("system clock is before Unix epoch"))?;
            let now_ms = u64::try_from(now.as_millis())
                .map_err(|_| HarnessError::execution("system clock overflow"))?;
            store.reset_authentication_settings(now_ms).await?;
            println!(
                "Saved login settings cleared. Turnstile is disabled; OIDC falls back to deployment configuration. Accounts and passwords are unchanged."
            );
        }
        Command::BackupSqlite {
            config,
            database_url,
            output,
        } => {
            let url = if let Some(url) = database_url {
                url
            } else {
                let path = config.map_or_else(crate::config::default_config_path, Ok)?;
                crate::config::ServerConfig::read(&path)?.database_url
            };
            crate::backup::sqlite_snapshot(&url, &output).await?;
            println!(
                "Consistent SQLite database snapshot saved to {}",
                output.display()
            );
            println!(
                "Keep the matching Server configuration and master key with this snapshot. Worker volumes are backed up separately."
            );
        }
        Command::RotateSecretMasterKey {
            database_url,
            current_key,
            next_key,
        } => {
            let database_url =
                Zeroizing::new(required_configuration(database_url, "database URL")?);
            let current_key = Zeroizing::new(required_configuration(current_key, "current key")?);
            let next_key = Zeroizing::new(required_configuration(next_key, "next key")?);
            let current_cipher = SecretCipher::from_base64(&current_key)?;
            let next_cipher = SecretCipher::from_base64(&next_key)?;
            let rotated = ControlStore::rotate_secret_master_key(
                &database_url,
                &current_cipher,
                &next_cipher,
            )
            .await?;
            println!("rotated {rotated} stored secret version(s)");
        }
    }
    Ok(())
}

fn required_configuration(value: String, label: &str) -> Result<String, HarnessError> {
    if value.trim().is_empty() {
        return Err(HarnessError::invalid(format!("{label} must not be empty")));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn rotation_options_expose_fixed_environment_and_direct_cli_entries() {
        let command = Args::command();
        let rotate = command
            .find_subcommand("rotate-secret-master-key")
            .expect("rotation subcommand");
        for (id, environment) in [
            ("database_url", "TERNILO_MIGRATION_DATABASE_URL"),
            ("current_key", "TERNILO_SECRET_MASTER_KEY"),
            ("next_key", "TERNILO_NEXT_SECRET_MASTER_KEY"),
        ] {
            let argument = rotate
                .get_arguments()
                .find(|argument| argument.get_id() == id)
                .unwrap_or_else(|| panic!("missing CLI argument {id}"));
            assert_eq!(argument.get_env(), Some(std::ffi::OsStr::new(environment)));
        }

        let parsed = Args::try_parse_from([
            "ternilo-server-admin",
            "rotate-secret-master-key",
            "--database-url",
            "postgres://migrator",
            "--current-key",
            "current",
            "--next-key",
            "next",
        ])
        .unwrap();
        let Command::RotateSecretMasterKey {
            database_url,
            current_key,
            next_key,
        } = parsed.command
        else {
            panic!("expected rotation command")
        };
        assert_eq!(database_url, "postgres://migrator");
        assert_eq!(current_key, "current");
        assert_eq!(next_key, "next");
    }

    #[test]
    fn rotation_environment_is_read_and_cli_wins() {
        const CHILD_ENV: &str = "TERNILO_CONTROL_ADMIN_ARGS_TEST_CHILD";
        if std::env::var(CHILD_ENV).as_deref() == Ok("1") {
            let from_env =
                Args::try_parse_from(["ternilo-server-admin", "rotate-secret-master-key"]).unwrap();
            let Command::RotateSecretMasterKey {
                database_url,
                current_key,
                next_key,
            } = from_env.command
            else {
                panic!("expected rotation command")
            };
            assert_eq!(database_url, "postgres://env-migrator");
            assert_eq!(current_key, "env-current");
            assert_eq!(next_key, "env-next");

            let from_cli = Args::try_parse_from([
                "ternilo-server-admin",
                "rotate-secret-master-key",
                "--database-url",
                "postgres://cli-migrator",
                "--current-key",
                "cli-current",
                "--next-key",
                "cli-next",
            ])
            .unwrap();
            let Command::RotateSecretMasterKey {
                database_url,
                current_key,
                next_key,
            } = from_cli.command
            else {
                panic!("expected rotation command")
            };
            assert_eq!(database_url, "postgres://cli-migrator");
            assert_eq!(current_key, "cli-current");
            assert_eq!(next_key, "cli-next");
            return;
        }

        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("admin::tests::rotation_environment_is_read_and_cli_wins")
            .arg("--exact")
            .env(CHILD_ENV, "1")
            .env("TERNILO_MIGRATION_DATABASE_URL", "postgres://env-migrator")
            .env("TERNILO_SECRET_MASTER_KEY", "env-current")
            .env("TERNILO_NEXT_SECRET_MASTER_KEY", "env-next")
            .status()
            .unwrap();
        assert!(status.success());
    }
}
