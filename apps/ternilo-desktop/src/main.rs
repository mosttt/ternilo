#![forbid(unsafe_code)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    error::Error,
    io,
    net::SocketAddr,
    path::PathBuf,
    process::{ExitCode, Stdio},
    sync::Mutex,
    time::Duration,
};

use clap::Parser;
use tauri::{Emitter, Manager, WebviewWindowBuilder};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use ternilo_local::default_data_dir;

const DEFAULT_DESKTOP_LISTEN: &str = "127.0.0.1:3210";

type AnyError = Box<dyn Error + Send + Sync + 'static>;

struct InitialLinks(Mutex<Vec<String>>);

#[derive(Parser)]
#[command(about = "Ternilo native desktop shell", version)]
struct Args {
    /// Loopback address used by the shared Local Web host.
    #[arg(
        long,
        env = "TERNILO_DESKTOP_LISTEN",
        default_value = DEFAULT_DESKTOP_LISTEN
    )]
    listen: SocketAddr,
    #[arg(
        long = "profile",
        env = "TERNILO_DESKTOP_PROFILES",
        value_delimiter = ','
    )]
    profile_layers: Vec<PathBuf>,
    #[arg(long, env = "TERNILO_DESKTOP_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Run the shared background service without creating a window.
    #[arg(long, hide = true)]
    service: bool,
    /// Isolate a smoke-test instance from an installed or already-running app.
    #[arg(long, hide = true)]
    test_instance: Option<String>,
    /// Deep link passed by the operating system on a cold start.
    #[arg(value_name = "DEEP_LINK")]
    deep_link: Option<String>,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ternilo-desktop: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<(), AnyError> {
    let Args {
        listen,
        profile_layers,
        data_dir,
        test_instance,
        deep_link,
        service,
    } = args;
    let application_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ternilo-desktop-core")
        .build()?;
    let data_dir = data_dir.map_or_else(default_data_dir, Ok)?;
    if listen.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "desktop listen address must use 127.0.0.1",
        )
        .into());
    }
    if service {
        return application_runtime
            .block_on(ternilo::service::run(ternilo::service::ServeOptions {
                listen: Some(listen),
                profile_layers,
                data_dir: Some(data_dir),
                max_steps: None,
                max_tool_calls: None,
                gateway_url: None,
                token: None,
                node_id: None,
                no_local_web: Some(false),
                allow_insecure_gateway: None,
            }))
            .map_err(Into::into);
    }
    let register_deep_links = test_instance.is_none();
    let mut context = tauri::generate_context!();
    if let Some(test_instance) = test_instance {
        context.config_mut().identifier = test_instance_identifier(&test_instance)?;
    }
    let setup_runtime = application_runtime.handle().clone();

    let desktop = tauri::Builder::default()
        .manage(InitialLinks(Mutex::new(deep_link.into_iter().collect())))
        // This plugin must remain first: a second deep-link launch exits before
        // it attempts to open the already-owned local application data store.
        .plugin(
            tauri_plugin_single_instance::Builder::new()
                .callback(|app, arguments, _working_directory| {
                    focus_main_window(app);
                    if let Some(link) = deep_link_argument(&arguments) {
                        let _ = app.emit("deep-link://new-url", vec![link]);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_deep_link::init())
        .invoke_handler(tauri::generate_handler![
            pick_workspace,
            desktop_notify,
            desktop_initial_links
        ])
        .setup(move |app| {
            let connection = setup_runtime
                .block_on(connect_local_service(&data_dir, listen, &profile_layers))
                .map_err(io::Error::other)?;
            let origin = connection.info.origin();
            println!("Ternilo local web: {origin}");

            let mut config = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == "main")
                .ok_or("main desktop window configuration is missing")?
                .clone();
            config.url = tauri::WebviewUrl::External(format!("{origin}/").parse()?);
            WebviewWindowBuilder::from_config(app.handle(), &config)?.build()?;

            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |_event| {
                focus_main_window(&handle);
            });
            if register_deep_links && let Err(error) = app.deep_link().register_all() {
                eprintln!("ternilo-desktop: deep-link registration failed: {error}");
            }
            Ok(())
        })
        .build(context);
    let desktop = desktop?;
    desktop.run(|_, _| {});
    Ok(())
}

async fn connect_local_service(
    data_dir: &std::path::Path,
    listen: SocketAddr,
    profiles: &[PathBuf],
) -> Result<ternilo::service::ServiceConnection, AnyError> {
    let _start_lock = ternilo::service::start_lock(data_dir).await?;
    if let Some(connection) = ternilo::service::discover(data_dir).await? {
        return require_browser(connection);
    }
    let log_path = data_dir.join("service.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .arg("--service")
        .arg("--listen")
        .arg(listen.to_string())
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    for profile in profiles {
        command.arg("--profile").arg(profile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn()?;
    for _ in 0..300 {
        if let Some(connection) = ternilo::service::discover(data_dir).await? {
            return require_browser(connection);
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "local service exited with {status}; see {}",
                log_path.display()
            ))
            .into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::other(format!(
        "local service startup timed out; see {}",
        log_path.display()
    ))
    .into())
}

fn require_browser(
    connection: ternilo::service::ServiceConnection,
) -> Result<ternilo::service::ServiceConnection, AnyError> {
    if connection.info.address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
        return Err(io::Error::other(
            "desktop native capabilities require 127.0.0.1; restart the local service with --listen 127.0.0.1:3210",
        )
        .into());
    }
    if connection.info.version != env!("CARGO_PKG_VERSION") {
        return Err(io::Error::other(
            "local service version differs; stop it before starting this desktop version",
        )
        .into());
    }
    if !connection.info.browser_enabled {
        return Err(io::Error::other("the local service has its browser UI disabled; restart without --no-local-web to use the desktop").into());
    }
    Ok(connection)
}

#[tauri::command]
async fn pick_workspace(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let selected = app
        .dialog()
        .file()
        .set_title("Choose a Ternilo workspace")
        .blocking_pick_folder();
    let Some(selected) = selected else {
        return Ok(None);
    };
    let path = selected
        .into_path()
        .map_err(|error| format!("Read selected path: {error}"))?;
    let path = std::fs::canonicalize(&path)
        .map_err(|error| format!("Resolve {}: {error}", path.display()))?;
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
fn desktop_notify(app: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    validate_notification_text(&title, 80, "notification title")?;
    validate_notification_text(&body, 240, "notification body")?;
    let result = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|error| format!("Send system notification: {error}"));
    drop(app);
    result
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri CommandArg requires State by value.
fn desktop_initial_links(
    app: tauri::AppHandle,
    initial_links: tauri::State<'_, InitialLinks>,
) -> Result<Vec<String>, String> {
    let mut links = initial_links
        .0
        .lock()
        .map_err(|_| "Read startup links: desktop state lock is poisoned".to_owned())?
        .drain(..)
        .collect::<Vec<_>>();
    let plugin_links = app
        .deep_link()
        .get_current()
        .map(|current| current.unwrap_or_default().into_iter().map(Into::into))
        .map_err(|error| format!("Read startup links: {error}"));
    drop(app);
    for link in plugin_links? {
        if !links.contains(&link) {
            links.push(link);
        }
    }
    Ok(links)
}

fn validate_notification_text(value: &str, maximum: usize, field: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if value.chars().count() > maximum {
        return Err(format!("{field} must not exceed {maximum} characters"));
    }
    Ok(())
}

fn focus_main_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

fn deep_link_argument(arguments: &[String]) -> Option<String> {
    arguments
        .iter()
        .skip(1)
        .find(|argument| argument.starts_with("ternilo://"))
        .cloned()
}

fn test_instance_identifier(instance: &str) -> Result<String, AnyError> {
    if instance.is_empty()
        || instance.len() > 64
        || !instance
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "test instance must contain 1-64 ASCII letters, digits or hyphens",
        )
        .into());
    }
    Ok(format!("dev.ternilo.desktop.test-{instance}"))
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::PathBuf};

    use clap::{CommandFactory, Parser};

    use super::{Args, deep_link_argument, test_instance_identifier, validate_notification_text};

    #[test]
    fn desktop_requires_an_origin_authorized_for_native_capabilities() {
        for (address, accepted) in [
            ("127.0.0.1:3210", true),
            ("127.0.0.1:4321", true),
            ("127.0.0.2:3210", false),
            ("[::1]:3210", false),
        ] {
            let connection = ternilo::service::ServiceConnection {
                info: ternilo::service::ServiceInfo {
                    service_id: "desktop-origin-test".to_owned(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    pid: 1,
                    address: address.parse().unwrap(),
                    browser_enabled: true,
                    remote_enabled: false,
                },
                api_token: "test-token".to_owned(),
            };
            assert_eq!(
                super::require_browser(connection).is_ok(),
                accepted,
                "{address}"
            );
        }
    }

    #[test]
    fn desktop_cli_metadata_is_valid_and_exposes_runtime_environment() {
        Args::command().debug_assert();
        let command = Args::command();
        for (id, environment) in [
            ("listen", "TERNILO_DESKTOP_LISTEN"),
            ("profile_layers", "TERNILO_DESKTOP_PROFILES"),
            ("data_dir", "TERNILO_DESKTOP_DATA_DIR"),
        ] {
            assert_eq!(
                command
                    .get_arguments()
                    .find(|argument| argument.get_id() == id)
                    .expect("desktop runtime argument")
                    .get_env(),
                Some(OsStr::new(environment))
            );
        }
        assert_eq!(
            command
                .get_arguments()
                .find(|argument| argument.get_id() == "profile_layers")
                .expect("profile argument")
                .get_value_delimiter(),
            Some(',')
        );
        for id in ["test_instance", "deep_link"] {
            assert_eq!(
                command
                    .get_arguments()
                    .find(|argument| argument.get_id() == id)
                    .expect("one-shot desktop argument")
                    .get_env(),
                None
            );
        }
    }

    #[test]
    fn desktop_runtime_environment_is_read_and_cli_wins() {
        const CHILD_ENV: &str = "TERNILO_DESKTOP_ARGS_TEST_CHILD";
        if std::env::var(CHILD_ENV).as_deref() == Ok("1") {
            let from_env = Args::try_parse_from(["ternilo-desktop"]).unwrap();
            assert_eq!(
                from_env.listen,
                "127.0.0.1:4325".parse::<std::net::SocketAddr>().unwrap()
            );
            assert_eq!(
                from_env.profile_layers,
                ["env-base.json", "env-desktop.json"].map(PathBuf::from)
            );
            assert_eq!(
                from_env.data_dir,
                Some(PathBuf::from("/var/lib/ternilo-desktop-env"))
            );
            assert!(from_env.test_instance.is_none());
            assert!(from_env.deep_link.is_none());

            let from_cli = Args::try_parse_from([
                "ternilo-desktop",
                "--listen",
                "127.0.0.1:0",
                "--profile",
                "base.json",
                "--profile",
                "desktop.json",
                "--data-dir",
                "/var/lib/ternilo-desktop",
                "--test-instance",
                "cli-test",
                "ternilo://workspace?path=%2Ftmp%2Fworkspace",
            ])
            .unwrap();
            assert_eq!(
                from_cli.listen,
                "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap()
            );
            assert_eq!(
                from_cli.profile_layers,
                ["base.json", "desktop.json"].map(PathBuf::from)
            );
            assert_eq!(
                from_cli.data_dir,
                Some(PathBuf::from("/var/lib/ternilo-desktop"))
            );
            assert_eq!(from_cli.test_instance.as_deref(), Some("cli-test"));
            assert_eq!(
                from_cli.deep_link.as_deref(),
                Some("ternilo://workspace?path=%2Ftmp%2Fworkspace")
            );
            return;
        }

        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("tests::desktop_runtime_environment_is_read_and_cli_wins")
            .arg("--exact")
            .env(CHILD_ENV, "1")
            .env("TERNILO_DESKTOP_LISTEN", "127.0.0.1:4325")
            .env("TERNILO_DESKTOP_PROFILES", "env-base.json,env-desktop.json")
            .env("TERNILO_DESKTOP_DATA_DIR", "/var/lib/ternilo-desktop-env")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn notification_text_is_trimmed_and_bounded() {
        assert!(validate_notification_text("  done  ", 8, "title").is_ok());
        assert!(validate_notification_text("   ", 8, "title").is_err());
        assert!(validate_notification_text("too long", 3, "title").is_err());
    }

    #[test]
    fn deep_link_is_forwarded_even_with_cli_options() {
        let arguments = vec![
            "ternilo-desktop".to_owned(),
            "--data-dir".to_owned(),
            "/tmp/ternilo".to_owned(),
            "ternilo://workspace?path=%2Ftmp%2Fworkspace".to_owned(),
        ];
        assert_eq!(
            deep_link_argument(&arguments).as_deref(),
            Some("ternilo://workspace?path=%2Ftmp%2Fworkspace")
        );
    }

    #[test]
    fn smoke_instance_uses_an_independent_application_identifier() {
        assert_eq!(
            test_instance_identifier("abc-123").unwrap(),
            "dev.ternilo.desktop.test-abc-123"
        );
        assert!(test_instance_identifier("").is_err());
        assert!(test_instance_identifier("not_separate").is_err());
    }
}
