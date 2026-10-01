use std::{process::Stdio, time::Duration};

use ternilo_protocol::HarnessError;

use super::ServiceConnection;

pub(super) async fn open_when_ready(connection: ServiceConnection) {
    if let Err(error) = open(&connection).await {
        eprintln!(
            "Could not open the browser: {error}. Open {} manually.",
            connection.info.origin()
        );
    }
}

async fn open(connection: &ServiceConnection) -> Result<(), HarnessError> {
    let client = super::client()?;
    let origin = connection.info.origin();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if client
                .get(format!("{origin}/api/v1/service"))
                .bearer_auth(&connection.api_token)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| HarnessError::unavailable("local web UI did not become ready"))?;

    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = tokio::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command.creation_flags(0x0800_0000);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = tokio::process::Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = tokio::process::Command::new("xdg-open");

    let status = command
        .arg(origin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| HarnessError::execution(format!("launch default browser: {error}")))?;
    if !status.success() {
        return Err(HarnessError::execution(format!(
            "browser launcher exited with {status}"
        )));
    }
    Ok(())
}
