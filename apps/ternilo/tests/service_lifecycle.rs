use std::{
    error::Error,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use ternilo::service::{ServiceConnection, ServiceInfo};
use ternilo_protocol::{LIVE_PROTOCOL_VERSION, LiveClientFrame, LiveServerFrame};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

struct ChildGuard {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl ChildGuard {
    fn spawn(mut command: Command, logs: &Path, name: &str) -> TestResult<Self> {
        let stdout = logs.join(format!("{name}.stdout"));
        let stderr = logs.join(format!("{name}.stderr"));
        command
            .stdin(Stdio::null())
            .stdout(File::create(&stdout)?)
            .stderr(File::create(&stderr)?);
        let child = command.spawn()?;
        Ok(Self {
            child,
            stdout,
            stderr,
        })
    }

    fn output(&self) -> TestResult<(String, String)> {
        Ok((
            fs::read_to_string(&self.stdout)?,
            fs::read_to_string(&self.stderr)?,
        ))
    }

    fn ensure_running(&mut self) -> TestResult {
        if let Some(status) = self.child.try_wait()? {
            let (stdout, stderr) = self.output()?;
            return Err(
                format!("service exited before readiness: {status}\n{stdout}\n{stderr}").into(),
            );
        }
        Ok(())
    }

    async fn wait_success(&mut self) -> TestResult<ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(35);
        loop {
            if let Some(status) = self.child.try_wait()? {
                let (stdout, stderr) = self.output()?;
                assert!(
                    status.success(),
                    "process failed: {status}\n{stdout}\n{stderr}"
                );
                return Ok(status);
            }
            if Instant::now() >= deadline {
                let (stdout, stderr) = self.output()?;
                return Err(format!("process did not exit gracefully\n{stdout}\n{stderr}").into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ternilo"));
    // A developer's remote connection or profile must not enter this fixture.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("TERNILO_LOCAL_") {
            command.env_remove(name);
        }
    }
    command
}

fn start_service(data: &Path, logs: &Path, name: &str, headless: bool) -> TestResult<ChildGuard> {
    let mut command = command();
    command.args(["serve", "--listen", "127.0.0.1:0", "--data-dir"]);
    command.arg(data);
    if headless {
        command.arg("--no-local-web");
    }
    ChildGuard::spawn(command, logs, name)
}

fn client() -> TestResult<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?)
}

async fn wait_ready(
    child: &mut ChildGuard,
    data: &Path,
    client: &Client,
) -> TestResult<ServiceConnection> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        child.ensure_running()?;
        match fs::read(data.join("runtime/service.json")) {
            Ok(bytes) => {
                let connection: ServiceConnection = serde_json::from_slice(&bytes)?;
                let response = client
                    .get(format!("{}/api/v1/service", connection.info.origin()))
                    .bearer_auth(&connection.api_token)
                    .send()
                    .await;
                if let Ok(response) = response
                    && response.status() == StatusCode::OK
                {
                    let info: ServiceInfo = response.json().await?;
                    assert_eq!(info, connection.info);
                    return Ok(connection);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            let (stdout, stderr) = child.output()?;
            return Err(format!("local service did not become ready\n{stdout}\n{stderr}").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn status(
    data: &Path,
    logs: &Path,
    name: &str,
    connection: &ServiceConnection,
) -> TestResult {
    let mut command = command();
    command.args(["status", "--data-dir"]).arg(data);
    let mut probe = ChildGuard::spawn(command, logs, name)?;
    probe.wait_success().await?;
    let (stdout, stderr) = probe.output()?;
    assert!(
        !stdout.contains(&connection.api_token),
        "status exposed the API token"
    );
    assert!(
        !stderr.contains(&connection.api_token),
        "status diagnostics exposed the API token"
    );
    let info: Value = serde_json::from_str(&stdout)?;
    assert_eq!(info, serde_json::to_value(&connection.info)?);
    Ok(())
}

async fn stop_http(client: &Client, connection: &ServiceConnection) -> TestResult {
    let response = client
        .post(format!("{}/api/v1/service/stop", connection.info.origin()))
        .bearer_auth(&connection.api_token)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    Ok(())
}

#[tokio::test]
async fn service_identity_authentication_status_and_graceful_restart() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let data = temporary.path().join("data");
    let client = client()?;
    let mut service = start_service(&data, temporary.path(), "first-service", false)?;
    let connection = wait_ready(&mut service, &data, &client).await?;
    assert_eq!(connection.info.pid, service.child.id());
    assert!(connection.info.address.ip().is_loopback());
    assert_ne!(connection.info.address.port(), 0);
    assert!(connection.info.browser_enabled);
    assert!(!connection.info.remote_enabled);
    assert!(!connection.api_token.is_empty());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(data.join("runtime/service.json"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let origin = connection.info.origin();
    let anonymous = client
        .get(format!("{origin}/api/v1/service"))
        .send()
        .await?;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let authenticated = client
        .get(format!("{origin}/api/v1/service"))
        .bearer_auth(&connection.api_token)
        .send()
        .await?;
    assert_eq!(authenticated.status(), StatusCode::OK);
    let identity: Value = authenticated.json().await?;
    assert_eq!(identity, serde_json::to_value(&connection.info)?);
    assert!(!identity.to_string().contains(&connection.api_token));
    assert_eq!(client.get(&origin).send().await?.status(), StatusCode::OK);
    status(&data, temporary.path(), "first-status", &connection).await?;

    let rejected = client
        .post(format!("{origin}/api/v1/service/stop"))
        .bearer_auth("incorrect-service-token")
        .send()
        .await?;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    service.ensure_running()?;
    status(&data, temporary.path(), "after-rejected-stop", &connection).await?;

    let live_url = format!("ws://{}/api/v1/live", connection.info.address);
    let (mut browser, _) = tokio_tungstenite::connect_async(&live_url).await?;
    browser
        .send(tokio_tungstenite::tungstenite::Message::Text(
            serde_json::to_string(&LiveClientFrame::Hello {
                protocol_version: LIVE_PROTOCOL_VERSION,
                bearer_token: Some(connection.api_token.clone()),
                tenant_id: None,
            })?
            .into(),
        ))
        .await?;
    let ready = tokio::time::timeout(Duration::from_secs(3), browser.next())
        .await?
        .ok_or("live socket closed before Ready")??;
    assert!(matches!(
        serde_json::from_str::<LiveServerFrame>(ready.to_text()?)?,
        LiveServerFrame::Ready { .. }
    ));
    // A second browser that has not sent Hello must also release its connection.
    let (pending_browser, _) = tokio_tungstenite::connect_async(&live_url).await?;

    let mut stop = command();
    stop.args(["stop", "--data-dir"]).arg(&data);
    let mut stop = ChildGuard::spawn(stop, temporary.path(), "stop-command")?;
    stop.wait_success().await?;
    assert!(!data.join("runtime/service.json").exists());
    let mut restarted = start_service(&data, temporary.path(), "restarted-service", false)?;
    let next = wait_ready(&mut restarted, &data, &client).await?;
    service.wait_success().await?;
    drop(browser);
    drop(pending_browser);
    let (stdout, stderr) = service.output()?;
    assert!(!stdout.contains(&connection.api_token));
    assert!(!stderr.contains(&connection.api_token));

    assert_ne!(next.info.service_id, connection.info.service_id);
    assert_ne!(next.api_token, connection.api_token);
    status(&data, temporary.path(), "restarted-status", &next).await?;
    stop_http(&client, &next).await?;
    restarted.wait_success().await?;
    assert!(!data.join("runtime/service.json").exists());
    Ok(())
}

#[tokio::test]
async fn headless_service_keeps_management_without_serving_the_browser() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let data = temporary.path().join("data");
    let client = client()?;
    let mut service = start_service(&data, temporary.path(), "headless-service", true)?;
    let connection = wait_ready(&mut service, &data, &client).await?;
    assert!(!connection.info.browser_enabled);
    assert!(!connection.info.remote_enabled);
    let rejected = command()
        .args(["serve", "--open-browser", "--data-dir"])
        .arg(&data)
        .output()?;
    assert!(!rejected.status.success());
    assert!(String::from_utf8(rejected.stderr)?.contains("requires the local web UI"));
    let origin = connection.info.origin();
    for route in [
        "/",
        "/offline.html",
        "/assets/app.js",
        "/manifest.webmanifest",
        "/api/v1/live",
    ] {
        let response = client
            .get(format!("{origin}{route}"))
            .bearer_auth(&connection.api_token)
            .send()
            .await?;
        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "headless route {route}: {}",
            response.status()
        );
    }
    let health = client
        .get(format!("{origin}/api/v1/health"))
        .bearer_auth(&connection.api_token)
        .send()
        .await?;
    assert_eq!(health.status(), StatusCode::OK);
    status(&data, temporary.path(), "headless-status", &connection).await?;
    stop_http(&client, &connection).await?;
    service.wait_success().await?;
    assert!(!data.join("runtime/service.json").exists());
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn browser_launch_uses_the_actual_origin_and_is_not_persisted() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir()?;
    let launcher = temporary.path().join("xdg-open");
    let opened = temporary.path().join("opened-urls");
    fs::write(
        &launcher,
        "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$TERNILO_TEST_BROWSER_URLS\"\n",
    )?;
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700))?;
    let mut paths = vec![temporary.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths)?;
    let client = client()?;
    for (index, flag) in ["--open-browser", "--open-brower"].into_iter().enumerate() {
        let data = temporary.path().join(format!("data-{index}"));
        let mut start = command();
        start
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(&data)
            .arg(flag)
            .env("PATH", &path)
            .env("TERNILO_TEST_BROWSER_URLS", &opened);
        let mut service = ChildGuard::spawn(start, temporary.path(), &format!("browser-{index}"))?;
        let connection = wait_ready(&mut service, &data, &client).await?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(urls) = fs::read_to_string(&opened)
                && urls.lines().last() == Some(connection.info.origin().as_str())
            {
                assert_eq!(urls.lines().count(), index + 1);
                assert!(!urls.contains(&connection.api_token));
                break;
            }
            service.ensure_running()?;
            assert!(Instant::now() < deadline, "browser launcher was not called");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            client.get(connection.info.origin()).send().await?.status(),
            StatusCode::OK
        );
        let config: Value = serde_json::from_slice(&fs::read(data.join("config.json"))?)?;
        assert!(config.get("open_browser").is_none());
        stop_http(&client, &connection).await?;
        service.wait_success().await?;

        let mut restart = command();
        restart
            .args(["serve", "--data-dir"])
            .arg(&data)
            .env("PATH", &path)
            .env("TERNILO_TEST_BROWSER_URLS", &opened);
        let mut restarted = ChildGuard::spawn(
            restart,
            temporary.path(),
            &format!("browser-restart-{index}"),
        )?;
        let next = wait_ready(&mut restarted, &data, &client).await?;
        stop_http(&client, &next).await?;
        restarted.wait_success().await?;
        assert_eq!(fs::read_to_string(&opened)?.lines().count(), index + 1);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn browser_launcher_failure_keeps_the_service_running() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir()?;
    let launcher = temporary.path().join("xdg-open");
    fs::write(&launcher, "#!/bin/sh\nexit 1\n")?;
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700))?;
    let data = temporary.path().join("data");
    let mut start = command();
    start
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(&data)
        .arg("--open-browser")
        .env("PATH", temporary.path());
    let mut service = ChildGuard::spawn(start, temporary.path(), "browser-failure")?;
    let client = client()?;
    let connection = wait_ready(&mut service, &data, &client).await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, stderr) = service.output()?;
        if stderr.contains("Could not open the browser") {
            assert!(stderr.contains(&connection.info.origin()));
            assert!(!stderr.contains(&connection.api_token));
            break;
        }
        service.ensure_running()?;
        assert!(
            Instant::now() < deadline,
            "missing browser failure diagnostic"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    service.ensure_running()?;
    assert_eq!(
        client.get(connection.info.origin()).send().await?.status(),
        StatusCode::OK
    );
    stop_http(&client, &connection).await?;
    service.wait_success().await?;
    Ok(())
}
