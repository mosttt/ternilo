use clap::Args;
use std::path::PathBuf;
use ternilo_control::{ControlStore, SecretCipher};
use ternilo_protocol::HarnessError;

#[derive(Args)]
pub(super) struct Options {
    #[arg(long, env = "TERNILO_SERVER_CONFIG_DIR")]
    config_dir: Option<PathBuf>,
    #[arg(long)]
    username: String,
}
pub(super) async fn execute(options: Options) -> Result<(), HarnessError> {
    let path = crate::config::configuration_path(options.config_dir.as_deref())?;
    let config = crate::config::ServerConfig::read(&path)?;
    let store = ControlStore::connect(
        &config.database_url,
        config.migration_database_url.as_deref(),
        SecretCipher::from_base64(&config.secret_master_key)?,
        config.max_database_connections,
    )
    .await?;
    let user = store
        .reset_native_mfa(&options.username, super::now_ms()?)
        .await?;
    println!(
        "MFA reset for {} ({}). Browser sessions and pending sign-ins were revoked.",
        user.username, user.user_id
    );
    store.database().close().await;
    Ok(())
}
