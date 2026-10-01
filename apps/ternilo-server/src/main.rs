#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ternilo_protocol::HarnessError;

mod admin;
mod assets;
mod backup;
mod config;
mod gateway_journal;
mod http;
mod platform;
mod setup;

#[derive(Parser)]
#[command(about = "Ternilo server for your computers and shared work", version)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    serve: config::ServeOptions,
}

#[derive(Subcommand)]
enum Command {
    /// Set up the database, private configuration, and first account.
    Init(setup::InitOptions),
    /// Start the server using its saved configuration.
    Serve(config::ServeOptions),
    /// Run backup and maintenance operations.
    Admin(admin::Args),
}

#[tokio::main]
async fn main() -> ExitCode {
    match execute(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ternilo-server: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn execute(args: Args) -> Result<(), HarnessError> {
    match args.command {
        Some(Command::Init(options)) => setup::execute(options).await,
        Some(Command::Serve(options)) => platform::execute(options.load()?).await,
        Some(Command::Admin(options)) => admin::execute(options).await,
        None => platform::execute(args.serve.load()?).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn startup_and_setup_do_not_require_a_product_mode() {
        Args::command().debug_assert();
        let start = Args::try_parse_from(["ternilo-server"]).unwrap();
        assert!(start.command.is_none());
        let start =
            Args::try_parse_from(["ternilo-server", "serve", "--config-dir", "instance"]).unwrap();
        assert!(matches!(start.command, Some(Command::Serve(_))));
        let setup = Args::try_parse_from([
            "ternilo-server",
            "init",
            "--config-dir",
            "instance",
            "--non-interactive",
        ])
        .unwrap();
        assert!(matches!(setup.command, Some(Command::Init(_))));
    }

    #[test]
    fn offline_secret_rotation_remains_available() {
        let rotation = Args::try_parse_from([
            "ternilo-server",
            "admin",
            "rotate-secret-master-key",
            "--database-url",
            "sqlite:server.sqlite3",
            "--current-key",
            "current",
            "--next-key",
            "next",
        ])
        .unwrap();
        assert!(matches!(rotation.command, Some(Command::Admin(_))));
    }
}
