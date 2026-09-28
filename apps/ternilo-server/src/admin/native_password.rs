use std::{
    io::{self, IsTerminal, Read},
    path::PathBuf,
};

use clap::Args;
use ternilo_control::{ControlStore, SecretCipher};
use ternilo_protocol::HarnessError;
use zeroize::Zeroizing;

#[derive(Args)]
pub(super) struct Options {
    #[arg(long, env = "TERNILO_SERVER_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long)]
    username: String,
    /// Read the new password from stdin instead of the hidden interactive prompts.
    #[arg(long)]
    password_stdin: bool,
}

pub(super) async fn execute(options: Options) -> Result<(), HarnessError> {
    let path = options
        .config
        .map_or_else(crate::config::default_config_path, Ok)?;
    let config = crate::config::ServerConfig::read(&path)?;
    let password = new_password(options.password_stdin)?;
    let store = ControlStore::connect(
        &config.database_url,
        config.migration_database_url.as_deref(),
        SecretCipher::from_base64(&config.secret_master_key)?,
        config.max_database_connections,
    )
    .await?;
    let reset = store
        .reset_native_password(&options.username, &password, super::now_ms()?)
        .await?;
    println!(
        "Password reset for {} ({}). Revoked {} native and {} OIDC browser sessions.",
        reset.user.username,
        reset.user.user_id,
        reset.native_sessions_revoked,
        reset.oidc_sessions_revoked,
    );
    store.database().close().await;
    Ok(())
}

fn new_password(from_stdin: bool) -> Result<Zeroizing<String>, HarnessError> {
    if from_stdin {
        let mut password = Zeroizing::new(String::new());
        // Allow the maximum password, a CRLF terminator, and one overflow byte.
        io::stdin()
            .lock()
            .take(1_027)
            .read_to_string(&mut password)
            .map_err(|_| HarnessError::invalid("cannot read a UTF-8 password from stdin"))?;
        if password.ends_with('\n') {
            password.pop();
            if password.ends_with('\r') {
                password.pop();
            }
        }
        return Ok(password);
    }
    if !io::stdin().is_terminal() {
        return Err(HarnessError::invalid(
            "use --password-stdin for non-interactive password recovery",
        ));
    }
    let password = Zeroizing::new(
        rpassword::prompt_password("New password: ")
            .map_err(|_| HarnessError::execution("read password from terminal"))?,
    );
    let repeated = Zeroizing::new(
        rpassword::prompt_password("Repeat new password: ")
            .map_err(|_| HarnessError::execution("read password confirmation from terminal"))?,
    );
    if password.as_str() != repeated.as_str() {
        return Err(HarnessError::invalid("passwords do not match"));
    }
    Ok(password)
}
