use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use std::{ffi::OsString, path::Path, process::Stdio, time::Duration};
use ternilo_kernel::{RunCancellation, SandboxMode, SandboxPolicy, SandboxesClient};
use ternilo_protocol::HarnessError;
use tokio::io::AsyncReadExt as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

mod math;

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
struct Cdp {
    socket: Socket,
    id: u64,
    session: Option<String>,
}
impl Cdp {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, HarnessError> {
        self.id += 1;
        let mut request = json!({"id":self.id,"method":method,"params":params});
        if let Some(session) = &self.session {
            request["sessionId"] = json!(session);
        }
        self.socket
            .send(Message::Text(request.to_string().into()))
            .await
            .map_err(failure)?;
        while let Some(message) = self.socket.next().await {
            let message = message.map_err(failure)?;
            if let Message::Text(text) = message {
                let response: Value = serde_json::from_str(&text).map_err(failure)?;
                if response["id"].as_u64() != Some(self.id) {
                    continue;
                }
                if !response["error"].is_null() {
                    return Err(HarnessError::execution(format!(
                        "document browser {}: {}",
                        method, response["error"]
                    )));
                }
                return Ok(response["result"].clone());
            }
        }
        Err(HarnessError::execution(
            "document browser connection closed",
        ))
    }
}
fn failure(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::execution(format!("document renderer: {error}"))
}

pub(crate) struct BrowserRender {
    pub height: u32,
    pub overflow: bool,
    pub missing_images: u32,
    pub blocked_resources: u32,
    pub screenshots: Vec<String>,
}

pub(crate) async fn render(
    sandboxes: &SandboxesClient,
    cancellation: &RunCancellation,
    workspace: &Path,
    browser: &str,
    html: String,
    width: u32,
    height: u32,
) -> Result<BrowserRender, HarnessError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let root = std::fs::canonicalize(directory.path()).map_err(failure)?;
    let profile = root.join("browser-profile");
    let arguments: Vec<OsString> = [
        "--headless=new".to_owned(),
        "--no-sandbox".into(),
        "--disable-gpu".into(),
        "--disable-dev-shm-usage".into(),
        "--disable-background-networking".into(),
        "--disable-breakpad".into(),
        "--disable-crashpad-for-testing".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--remote-debugging-port=0".into(),
        "--remote-debugging-address=127.0.0.1".into(),
        format!("--user-data-dir={}", profile.display()),
        "about:blank".into(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let cloud =
        std::env::var_os("TERNILO_OUTER_SANDBOX").as_deref() == Some(std::ffi::OsStr::new("1"));
    let confined = sandboxes
        .confine(
            OsString::from(browser),
            arguments,
            SandboxPolicy {
                scratch_root: None,
                mode: SandboxMode::WorkspaceWrite,
                workspace_root: if cloud {
                    workspace.to_path_buf()
                } else {
                    root.clone()
                },
            },
        )
        .await?;
    let mut command = tokio::process::Command::new(confined.program);
    command
        .args(confined.arguments)
        .env_clear()
        .envs(confined.environment)
        .env("HOME", &root)
        .env("LANG", "C.UTF-8")
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    for name in ["SYSTEMROOT", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let (mut child, group) = crate::process_supervision::spawn(&mut command).map_err(|error| {
        HarnessError::execution(format!(
            "cannot start document browser {browser:?}: {error}"
        ))
    })?;
    let stderr = child
        .stderr
        .take()
        .expect("document browser stderr is piped");
    let diagnostics = tokio::spawn(browser_diagnostics(stderr));
    let result = tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(HarnessError::cancelled("document rendering cancelled")),
        status = child.wait() => Err(HarnessError::execution(format!("document browser exited before rendering: {status:?}"))),
        result = tokio::time::timeout(Duration::from_secs(45), capture(&profile, html, width, height)) => result.map_err(|_| HarnessError::execution("document rendering exceeded 45 seconds")).and_then(|value| value),
    };
    group.terminate();
    let _ = child.start_kill();
    let _ = child.wait().await;
    group.wait_quiescent().await?;
    let diagnostics = diagnostics.await.unwrap_or_default();
    result.map_err(|mut error| {
        if !diagnostics.trim().is_empty() {
            error.message.push_str("\nBrowser diagnostics: ");
            error.message.push_str(diagnostics.trim());
        }
        error
    })
}

async fn browser_diagnostics(mut stderr: tokio::process::ChildStderr) -> String {
    let mut bytes = Vec::new();
    let _ = (&mut stderr).take(4096).read_to_end(&mut bytes).await;
    let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn capture(
    profile: &Path,
    html: String,
    width: u32,
    height: u32,
) -> Result<BrowserRender, HarnessError> {
    let endpoint = wait_endpoint(profile).await?;
    let (socket, _) = tokio_tungstenite::connect_async(endpoint)
        .await
        .map_err(failure)?;
    let mut cdp = Cdp {
        socket,
        id: 0,
        session: None,
    };
    let target = cdp
        .call("Target.createTarget", json!({"url":"about:blank"}))
        .await?;
    let attached = cdp
        .call(
            "Target.attachToTarget",
            json!({"targetId":target["targetId"],"flatten":true}),
        )
        .await?;
    cdp.session = Some(
        attached["sessionId"]
            .as_str()
            .ok_or_else(|| HarnessError::execution("document browser did not attach"))?
            .to_owned(),
    );
    cdp.call("Page.enable", json!({})).await?;
    cdp.call("Network.enable", json!({})).await?;
    cdp.call(
        "Network.setBlockedURLs",
        json!({"urls":["http://*","https://*","file://*"]}),
    )
    .await?;
    cdp.call(
        "Emulation.setDeviceMetricsOverride",
        json!({"width":width,"height":height,"deviceScaleFactor":1,"mobile":false}),
    )
    .await?;
    let frame = cdp.call("Page.getFrameTree", json!({})).await?;
    cdp.call(
        "Page.setDocumentContent",
        json!({"frameId":frame["frameTree"]["frame"]["id"],"html":html}),
    )
    .await?;
    if html.contains("data-ternilo-math=") {
        math::render(&mut cdp, &frame["frameTree"]["frame"]["id"]).await?;
    }
    let inspected = cdp.call("Runtime.evaluate", json!({"expression": r#"(async () => {
      await document.fonts.ready;
      await Promise.all([...document.images].map(image => image.decode().catch(() => null)));
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      const root = document.documentElement;
      return { marker: Boolean(document.querySelector('meta[name="ternilo-document-render"]')),
        height: Math.max(root.scrollHeight, document.body?.scrollHeight || 0),
        overflow: root.scrollWidth > innerWidth + 1,
        missingImages: [...document.images].filter(image => !image.complete || image.naturalWidth === 0).length,
        blockedResources: [...document.querySelectorAll('[src],link[href],image[href],use[href]')].filter(element => {
          const source = element.getAttribute('src') || element.getAttribute('href') || '';
          return source && !source.startsWith('data:') && !source.startsWith('#');
        }).length + document.querySelectorAll('script:not([src]),iframe,object,embed').length };
    })()"#, "awaitPromise":true,"returnByValue":true})).await?;
    let facts = &inspected["result"]["value"];
    if facts["marker"] != true {
        return Err(HarnessError::execution(
            "document navigated away during rendering",
        ));
    }
    let content_height = facts["height"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| HarnessError::execution("document browser did not return layout metrics"))?;
    let screenshots = capture_screenshots(&mut cdp, width, height, content_height).await?;
    Ok(BrowserRender {
        height: content_height,
        overflow: facts["overflow"] == true,
        missing_images: u32::try_from(facts["missingImages"].as_u64().unwrap_or(0))
            .unwrap_or(u32::MAX),
        blocked_resources: u32::try_from(facts["blockedResources"].as_u64().unwrap_or(0))
            .unwrap_or(u32::MAX),
        screenshots,
    })
}

async fn capture_screenshots(
    cdp: &mut Cdp,
    width: u32,
    height: u32,
    content_height: u32,
) -> Result<Vec<String>, HarnessError> {
    let pages = content_height.max(1).div_ceil(height);
    if pages > 8 {
        return Err(HarnessError::invalid(
            "document exceeds eight viewport pages; split it into smaller documents or increase viewport height before review",
        ));
    }
    let mut screenshots = Vec::new();
    let mut bytes = 0;
    for page in 0..pages {
        let y = page * height;
        let page_height = height.min(content_height.saturating_sub(y)).max(1);
        let captured = cdp.call("Page.captureScreenshot", json!({"format":"png","captureBeyondViewport":true,"clip":{"x":0,"y":y,"width":width,"height":page_height,"scale":1}})).await?;
        let data = captured["data"]
            .as_str()
            .ok_or_else(|| HarnessError::execution("document browser did not return an image"))?;
        bytes += data.len();
        if bytes > 8 * 1024 * 1024 {
            return Err(HarnessError::invalid(
                "document screenshots exceed 8 MiB; split the document before review",
            ));
        }
        screenshots.push(format!("data:image/png;base64,{data}"));
    }
    Ok(screenshots)
}

async fn wait_endpoint(profile: &Path) -> Result<String, HarnessError> {
    loop {
        if let Ok(value) = tokio::fs::read_to_string(profile.join("DevToolsActivePort")).await {
            let mut lines = value.lines();
            // Chromium creates this file before writing its port and endpoint.
            // A readable but incomplete file does not mean the browser is ready.
            if let (Some(port), Some(path)) = (
                lines.next().and_then(|value| value.parse::<u16>().ok()),
                lines.next(),
            ) && port != 0
                && path.starts_with('/')
            {
                return Ok(format!("ws://127.0.0.1:{port}{path}"));
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn endpoint_waits_for_chromium_to_finish_writing_its_port_file() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("DevToolsActivePort");
        tokio::fs::write(&file, "").await.unwrap();
        let waiting = wait_endpoint(directory.path());
        tokio::pin!(waiting);
        for partial in ["", "39523\n"] {
            tokio::fs::write(&file, partial).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(60), &mut waiting)
                    .await
                    .is_err()
            );
        }
        tokio::fs::write(&file, "39523\n/devtools/browser/render-test")
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .unwrap()
                .unwrap(),
            "ws://127.0.0.1:39523/devtools/browser/render-test",
        );
    }
}
