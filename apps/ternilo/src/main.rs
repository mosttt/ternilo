#![forbid(unsafe_code)]

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use ternilo_kernel::{HarnessSession, HostEnvironment, HostPolicy};
use ternilo_local::default_data_dir;
use ternilo_protocol::{
    AgentId, ExtensionProviderMaterializeRequest, HarnessError, Profile, RunId, RunLimits,
    SessionId, SessionIdentity, TenantId, UserId, WorkspaceBinding, WorkspaceId,
};

#[derive(Parser)]
#[command(about = "Ternilo local agent harness", version)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, Debug, clap::Args)]
struct ExecutionLimits {
    /// Host ceiling for Agent steps per turn; 0 means no host ceiling.
    #[arg(long, env = "TERNILO_LOCAL_MAX_STEPS", default_value_t = 0)]
    max_steps: u32,
    /// Host ceiling for tool calls per turn; 0 leaves the Agent configuration unrestricted.
    #[arg(long, env = "TERNILO_LOCAL_MAX_TOOL_CALLS", default_value_t = 0)]
    max_tool_calls: u32,
}

impl ExecutionLimits {
    fn run_limits(self) -> RunLimits {
        RunLimits {
            max_steps: self.max_steps,
            max_tool_calls: self.max_tool_calls,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Start the local browser UI (default).
    Serve(ternilo::service::ServeOptions),
    /// Show the running local service for a data directory.
    Status {
        #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
        data_dir: Option<PathBuf>,
    },
    /// Stop the local service and its running tasks gracefully.
    Stop {
        #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
        data_dir: Option<PathBuf>,
    },
    /// Run one prompt through the same shared harness API.
    Run {
        prompt: String,
        #[arg(
            long = "profile",
            env = "TERNILO_LOCAL_PROFILES",
            value_delimiter = ','
        )]
        profile_layers: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        limits: ExecutionLimits,
    },
    /// Serve the versioned JSON-RPC automation protocol over NDJSON stdio.
    Rpc {
        #[arg(
            long = "profile",
            env = "TERNILO_LOCAL_PROFILES",
            value_delimiter = ','
        )]
        profile_layers: Vec<PathBuf>,
        #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
        data_dir: Option<PathBuf>,
        #[command(flatten)]
        limits: ExecutionLimits,
    },
    /// Serve Agent Client Protocol v1 over protocol-pure JSON-RPC stdio.
    Acp {
        #[arg(
            long = "profile",
            env = "TERNILO_LOCAL_PROFILES",
            value_delimiter = ','
        )]
        profile_layers: Vec<PathBuf>,
        #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
        data_dir: Option<PathBuf>,
        #[command(flatten)]
        limits: ExecutionLimits,
    },
    /// Print the statically linked plugin catalog.
    Plugins,
    /// Manage local model Providers.
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
}

#[derive(Subcommand)]
enum ProviderCommand {
    /// Create a Provider from an enabled, trusted Extension template.
    AddFromExtension {
        #[arg(long, env = "TERNILO_PROVIDER_PACKAGE_ID")]
        package_id: String,
        #[arg(long, env = "TERNILO_PROVIDER_PACKAGE_VERSION")]
        version: String,
        #[arg(long, env = "TERNILO_PROVIDER_TEMPLATE")]
        template: String,
        #[arg(long, env = "TERNILO_PROVIDER_ID")]
        provider_id: String,
        #[arg(long, env = "TERNILO_PROVIDER_API_KEY_REF")]
        api_key_ref: Option<String>,
        #[arg(
            long = "profile",
            env = "TERNILO_LOCAL_PROFILES",
            value_delimiter = ','
        )]
        profile_layers: Vec<PathBuf>,
        #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
        data_dir: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match execute(Args::parse_from(arguments_with_default_serve(
        std::env::args_os(),
    )))
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn arguments_with_default_serve<I, T>(arguments: I) -> Vec<OsString>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
    if arguments.len() == 1 {
        arguments.push(OsString::from("serve"));
    }
    arguments
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep command dispatch and each command lifecycle together."
)]
async fn execute(args: Args) -> Result<(), HarnessError> {
    let command = args.command.ok_or_else(|| {
        HarnessError::invalid("local command was not normalized to the default serve command")
    })?;
    match command {
        Command::Serve(options) => ternilo::service::run(options).await,
        Command::Status { data_dir } => {
            let data_dir = data_dir.map_or_else(default_data_dir, Ok)?;
            let connection = ternilo::service::discover(&data_dir)
                .await?
                .ok_or_else(|| {
                    HarnessError::invalid("no local service is running for this data directory")
                })?;
            println!(
                "{}",
                serde_json::to_string_pretty(&connection.info)
                    .map_err(|error| HarnessError::execution(error.to_string()))?
            );
            Ok(())
        }
        Command::Stop { data_dir } => {
            ternilo::service::stop(&data_dir.map_or_else(default_data_dir, Ok)?).await
        }
        Command::Run {
            prompt,
            profile_layers,
            json,
            limits,
        } => {
            run_once(
                prompt,
                ternilo::load_local_profile(&profile_layers)?,
                json,
                limits.run_limits(),
            )
            .await
        }
        Command::Rpc {
            profile_layers,
            data_dir,
            limits,
        } => {
            let application = ternilo::open_local_application_with_limits(
                ternilo::load_local_profile(&profile_layers)?,
                data_dir.map_or_else(default_data_dir, Ok)?,
                limits.run_limits(),
            )
            .await?;
            ternilo_automation::serve_stdio(application).await
        }
        Command::Acp {
            profile_layers,
            data_dir,
            limits,
        } => {
            let application = ternilo::open_local_application_with_limits(
                ternilo::load_local_profile(&profile_layers)?,
                data_dir.map_or_else(default_data_dir, Ok)?,
                limits.run_limits(),
            )
            .await?;
            ternilo_acp::serve_stdio(application).await
        }
        Command::Plugins => {
            let catalog = ternilo_local::catalog()?;
            for kind in catalog.kinds() {
                println!("{kind}");
            }
            Ok(())
        }
        Command::Provider {
            command:
                ProviderCommand::AddFromExtension {
                    package_id,
                    version,
                    template,
                    provider_id,
                    api_key_ref,
                    profile_layers,
                    data_dir,
                },
        } => {
            let application = ternilo::open_local_application(
                ternilo::load_local_profile(&profile_layers)?,
                data_dir.map_or_else(default_data_dir, Ok)?,
            )
            .await?;
            let result = application
                .materialize_extension_provider(ExtensionProviderMaterializeRequest {
                    package_id,
                    version,
                    template,
                    provider_id,
                    api_key_ref,
                })
                .await;
            let shutdown = application.shutdown().await;
            let provider = result?;
            shutdown?;
            println!(
                "{}",
                serde_json::to_string_pretty(&provider).map_err(|error| {
                    HarnessError::execution(format!("serialize Provider profile: {error}"))
                })?
            );
            Ok(())
        }
    }
}

async fn run_once(
    prompt: String,
    profile: Profile,
    json: bool,
    limits: RunLimits,
) -> Result<(), HarnessError> {
    let catalog = ternilo_local::catalog()?;
    let cwd = std::fs::canonicalize(".")
        .map_err(|error| HarnessError::execution(format!("resolve current directory: {error}")))?;
    let execution = ternilo_local::DirectoryCoordinator::for_user()?
        .bind_user("local-user".to_owned(), cwd.clone());
    let environment = HostEnvironment::memory(
        local_identity("cli-session"),
        Some(WorkspaceBinding {
            workspace_id: WorkspaceId::new("cli"),
            path: cwd.to_string_lossy().into_owned(),
        }),
        HostPolicy::local(limits),
    )
    .with_workspace_execution(execution);
    let harness = HarnessSession::boot(&catalog, &profile, environment).await?;
    let outcome = harness
        .run_input(ternilo_protocol::AgentInput {
            additional_inputs: Vec::new(),
            run_id: RunId::new("cli-run"),
            input: prompt,
            provenance: Some(ternilo_protocol::InputProvenance {
                run_id: None,
                input_id: ternilo_protocol::SubmissionId::new("cli-input"),
                author: ternilo_protocol::InputAuthor::Local,
            }),
            display_input: None,
            source: None,
            references: Vec::new(),
            reference_contexts: Vec::new(),
            attachments: Vec::new(),
        })
        .await;
    let shutdown = harness.shutdown().await;
    let outcome = outcome?;
    shutdown?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome)
                .map_err(|error| HarnessError::execution(format!("serialize outcome: {error}")))?
        );
    } else {
        println!("{}", outcome.answer);
    }
    Ok(())
}

fn local_identity(session: impl Into<String>) -> SessionIdentity {
    SessionIdentity {
        tenant_id: TenantId::new("local"),
        user_id: UserId::new("local-user"),
        agent_id: AgentId::new("default"),
        session_id: SessionId::new(session),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    use clap::CommandFactory;

    #[test]
    fn local_cli_metadata_is_valid_and_exposes_runtime_environment() {
        Args::command().debug_assert();
        let command = Args::command();
        for subcommand_name in ["serve", "run", "rpc", "acp"] {
            let subcommand = command
                .find_subcommand(subcommand_name)
                .expect("local execution subcommand");
            let profile = subcommand
                .get_arguments()
                .find(|argument| argument.get_id() == "profile_layers")
                .expect("profile argument");
            assert_eq!(
                profile.get_env(),
                Some(OsStr::new("TERNILO_LOCAL_PROFILES"))
            );
            assert_eq!(profile.get_value_delimiter(), Some(','));
            assert_eq!(
                subcommand
                    .get_arguments()
                    .find(|argument| argument.get_id() == "max_steps")
                    .expect("max-steps argument")
                    .get_env(),
                Some(OsStr::new("TERNILO_LOCAL_MAX_STEPS"))
            );
            assert_eq!(
                subcommand
                    .get_arguments()
                    .find(|argument| argument.get_id() == "max_tool_calls")
                    .expect("max-tool-calls argument")
                    .get_env(),
                Some(OsStr::new("TERNILO_LOCAL_MAX_TOOL_CALLS"))
            );
        }
        let serve = command.find_subcommand("serve").expect("serve subcommand");
        assert_eq!(
            serve
                .get_arguments()
                .find(|argument| argument.get_id() == "listen")
                .expect("listen argument")
                .get_env(),
            Some(OsStr::new("TERNILO_LOCAL_LISTEN"))
        );
        for subcommand_name in ["serve", "rpc", "acp"] {
            assert_eq!(
                command
                    .find_subcommand(subcommand_name)
                    .expect("persistent local subcommand")
                    .get_arguments()
                    .find(|argument| argument.get_id() == "data_dir")
                    .expect("data-dir argument")
                    .get_env(),
                Some(OsStr::new("TERNILO_LOCAL_DATA_DIR"))
            );
        }
        let run = command.find_subcommand("run").expect("run subcommand");
        for id in ["prompt", "json"] {
            assert_eq!(
                run.get_arguments()
                    .find(|argument| argument.get_id() == id)
                    .expect("one-shot run argument")
                    .get_env(),
                None
            );
        }
        let add_from_extension = command
            .find_subcommand("provider")
            .expect("provider subcommand")
            .find_subcommand("add-from-extension")
            .expect("Provider materialization subcommand");
        for (id, environment) in [
            ("package_id", "TERNILO_PROVIDER_PACKAGE_ID"),
            ("version", "TERNILO_PROVIDER_PACKAGE_VERSION"),
            ("template", "TERNILO_PROVIDER_TEMPLATE"),
            ("provider_id", "TERNILO_PROVIDER_ID"),
            ("api_key_ref", "TERNILO_PROVIDER_API_KEY_REF"),
            ("profile_layers", "TERNILO_LOCAL_PROFILES"),
            ("data_dir", "TERNILO_LOCAL_DATA_DIR"),
        ] {
            assert_eq!(
                add_from_extension
                    .get_arguments()
                    .find(|argument| argument.get_id() == id)
                    .unwrap_or_else(|| panic!("missing {id} argument"))
                    .get_env(),
                Some(OsStr::new(environment))
            );
        }
    }

    #[test]
    fn provider_materialization_cli_accepts_only_user_owned_fields() {
        let args = Args::try_parse_from([
            "ternilo",
            "provider",
            "add-from-extension",
            "--package-id",
            "dev.example.models",
            "--version",
            "1.0.0",
            "--template",
            "primary",
            "--provider-id",
            "example-models",
            "--api-key-ref",
            "EXAMPLE_API_KEY",
            "--data-dir",
            "/tmp/ternilo-cli-test",
        ])
        .unwrap();
        let Some(Command::Provider {
            command:
                ProviderCommand::AddFromExtension {
                    package_id,
                    version,
                    template,
                    provider_id,
                    api_key_ref,
                    ..
                },
        }) = args.command
        else {
            panic!("expected Provider materialization command")
        };
        assert_eq!(package_id, "dev.example.models");
        assert_eq!(version, "1.0.0");
        assert_eq!(template, "primary");
        assert_eq!(provider_id, "example-models");
        assert_eq!(api_key_ref.as_deref(), Some("EXAMPLE_API_KEY"));
    }

    #[test]
    fn empty_command_line_is_parsed_as_serve() {
        let arguments = arguments_with_default_serve(["ternilo"]);
        assert_eq!(
            arguments,
            [OsString::from("ternilo"), OsString::from("serve")]
        );
        let args = Args::try_parse_from(arguments).unwrap();
        assert!(matches!(args.command, Some(Command::Serve(_))));
    }

    #[test]
    fn local_runtime_environment_is_read_and_cli_wins() {
        const CHILD_ENV: &str = "TERNILO_LOCAL_ARGS_TEST_CHILD";
        if std::env::var(CHILD_ENV).as_deref() == Ok("1") {
            let from_env = Args::try_parse_from(arguments_with_default_serve(["ternilo"])).unwrap();
            let Some(Command::Serve(ternilo::service::ServeOptions {
                listen,
                profile_layers,
                data_dir,
                max_steps,
                max_tool_calls,
                ..
            })) = from_env.command
            else {
                panic!("expected default serve command");
            };
            assert_eq!(
                listen,
                "127.0.0.1:4320".parse::<std::net::SocketAddr>().unwrap()
            );
            assert_eq!(
                profile_layers,
                [
                    PathBuf::from("env-base.json"),
                    PathBuf::from("env-host.json")
                ]
            );
            assert_eq!(data_dir, Some(PathBuf::from("/var/lib/ternilo-env")));
            assert_eq!(max_steps, 11);
            assert_eq!(max_tool_calls, 128);

            let from_cli = Args::try_parse_from([
                "ternilo",
                "serve",
                "--listen",
                "127.0.0.1:4321",
                "--profile",
                "base.json",
                "--profile",
                "machine.json",
                "--data-dir",
                "/var/lib/ternilo-local",
                "--max-steps",
                "17",
                "--max-tool-calls",
                "1024",
            ])
            .unwrap();
            let Some(Command::Serve(ternilo::service::ServeOptions {
                listen,
                profile_layers,
                data_dir,
                max_steps,
                max_tool_calls,
                ..
            })) = from_cli.command
            else {
                panic!("expected explicit serve command");
            };
            assert_eq!(
                listen,
                "127.0.0.1:4321".parse::<std::net::SocketAddr>().unwrap()
            );
            assert_eq!(
                profile_layers,
                [PathBuf::from("base.json"), PathBuf::from("machine.json")]
            );
            assert_eq!(data_dir, Some(PathBuf::from("/var/lib/ternilo-local")));
            assert_eq!(max_steps, 17);
            assert_eq!(max_tool_calls, 1024);
            return;
        }

        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("tests::local_runtime_environment_is_read_and_cli_wins")
            .arg("--exact")
            .env(CHILD_ENV, "1")
            .env("TERNILO_LOCAL_LISTEN", "127.0.0.1:4320")
            .env("TERNILO_LOCAL_PROFILES", "env-base.json,env-host.json")
            .env("TERNILO_LOCAL_DATA_DIR", "/var/lib/ternilo-env")
            .env("TERNILO_LOCAL_MAX_STEPS", "11")
            .env("TERNILO_LOCAL_MAX_TOOL_CALLS", "128")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn local_execution_commands_default_to_unlimited_steps() {
        for command in ["serve", "run", "rpc", "acp"] {
            let mut arguments = vec!["ternilo", command];
            if command == "run" {
                arguments.push("hello");
            }
            let args = Args::try_parse_from(arguments).unwrap();
            assert_eq!(
                command_limits(args.command.unwrap()).max_steps,
                0,
                "{command}"
            );
        }
    }

    #[test]
    fn local_execution_commands_accept_an_explicit_step_limit() {
        for command in ["serve", "run", "rpc", "acp"] {
            let mut arguments = vec!["ternilo", command];
            if command == "run" {
                arguments.push("hello");
            }
            arguments.extend(["--max-steps", "17"]);
            let args = Args::try_parse_from(arguments).unwrap();
            assert_eq!(
                command_limits(args.command.unwrap()).max_steps,
                17,
                "{command}"
            );
        }
    }

    #[test]
    fn local_execution_commands_support_default_and_custom_tool_call_limits() {
        for command in ["serve", "run", "rpc", "acp"] {
            let mut arguments = vec!["ternilo", command];
            if command == "run" {
                arguments.push("hello");
            }
            let defaults = Args::try_parse_from(arguments.clone()).unwrap();
            assert_eq!(
                command_limits(defaults.command.unwrap()).max_tool_calls,
                0,
                "{command}"
            );
            arguments.extend(["--max-tool-calls", "7", "--max-steps", "17"]);
            let explicit = Args::try_parse_from(arguments).unwrap();
            assert_eq!(
                command_limits(explicit.command.unwrap()),
                RunLimits {
                    max_steps: 17,
                    max_tool_calls: 7
                },
                "{command}"
            );
            let mut unlimited = vec!["ternilo", command, "--max-tool-calls", "0"];
            if command == "run" {
                unlimited.push("hello");
            }
            let unlimited = Args::try_parse_from(unlimited).unwrap();
            assert_eq!(
                command_limits(unlimited.command.unwrap()).max_tool_calls,
                0,
                "{command} accepts an unlimited host ceiling"
            );
        }
    }

    fn command_limits(command: Command) -> RunLimits {
        match command {
            Command::Serve(options) => options.run_limits(),
            Command::Run { limits, .. }
            | Command::Rpc { limits, .. }
            | Command::Acp { limits, .. } => limits.run_limits(),
            Command::Plugins
            | Command::Provider { .. }
            | Command::Status { .. }
            | Command::Stop { .. } => {
                panic!("non-execution commands have no execution limits")
            }
        }
    }
}
