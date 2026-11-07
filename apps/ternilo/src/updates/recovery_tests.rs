//! Filesystem failures must not turn an otherwise recoverable update into an outage.
use super::*;
use crate::updates::{UpdateRequest, installation::Installation};
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    root: tempfile::TempDir,
    plan: ApplyPlan,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let staging = root.path().join("staging");
        let data = root.path().join("data");
        for (directory, version) in [(&bin, "1.0.0"), (&staging, "1.0.1")] {
            fs::create_dir_all(directory).unwrap();
            for name in ternilo_update::executable_names(current_target().unwrap()) {
                let path = directory.join(name);
                fs::write(&path, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'ternilo {version}\\n'; else printf '{version}' > \"$1\"; fi\n")).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        fs::write(bin.join(".ternilo-update.lock"), "").unwrap();
        let settings = UpdateSettings {
            installation: Installation::Flat { bin },
            restart: RestartMethod::Direct,
        };
        write_private(&data.join("config/client-updates.json"), &settings).unwrap();
        let plan = ApplyPlan {
            request: UpdateRequest {
                id: "recovery-fixture".into(),
                manifest: String::new(),
                signature: String::new(),
                server_version: "1.0.1".into(),
                download_token: String::new(),
                expires_at_ms: u64::MAX,
            },
            settings,
            data_dir: data,
            archive: root.path().join("unused"),
            service_id: String::new(),
            backup: root.path().join("backup"),
            previous_version: "1.0.0".into(),
            arguments: vec![root.path().join("restarted").to_str().unwrap().into()],
        };
        write_private(
            &plan.data_dir.join("data/updates/progress.json"),
            &ternilo_protocol::ClientUpdateProgress {
                id: plan.request.id.clone(),
                version: "1.0.1".into(),
                phase: ClientUpdatePhase::Installing,
                revision: 1,
                error: None,
            },
        )
        .unwrap();
        Self { root, plan }
    }
    async fn activate(&self) -> Result<(), HarnessError> {
        activate(
            &self.plan,
            &self.root.path().join("staging"),
            "1.0.1",
            current_target().unwrap(),
        )
        .await
    }
    async fn assert_original_restarted(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while fs::read_to_string(self.root.path().join("restarted"))
                .ok()
                .as_deref()
                != Some("1.0.0")
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the original executable must restart even when reporting fails");
        assert!(
            !self
                .plan
                .settings
                .installation
                .control_dir()
                .join(".ternilo-installing.json")
                .exists()
        );
        probe_version(
            &self.plan.settings.installation.bin().join(executable()),
            "1.0.0",
        )
        .await
        .unwrap();
        let lock = installation_lock(&self.plan).await.unwrap();
        drop(lock);
    }
}

struct ReadOnly(std::path::PathBuf);
impl ReadOnly {
    fn set(path: &Path) -> Option<Self> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o500)).unwrap();
        let guard = Self(path.to_owned());
        // Root bypasses mode bits. Keep the fault meaningful rather than testing a writable path.
        if tempfile::NamedTempFile::new_in(path).is_ok() {
            eprintln!("read-only injection unavailable for this user");
            return None;
        }
        Some(guard)
    }
}
impl Drop for ReadOnly {
    fn drop(&mut self) {
        fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[tokio::test]
async fn marker_failure_restarts_without_replacing_programs() {
    let fixture = Fixture::new();
    let Some(_readonly) = ReadOnly::set(fixture.plan.settings.installation.control_dir()) else {
        return;
    };
    assert!(fixture.activate().await.is_err());
    fixture.assert_original_restarted().await;
    assert!(!fixture.plan.backup.exists());
}

#[tokio::test]
async fn progress_write_failure_rolls_back_and_restarts() {
    let fixture = Fixture::new();
    let Some(_readonly) = ReadOnly::set(&fixture.plan.data_dir.join("data/updates")) else {
        return;
    };
    assert!(fixture.activate().await.is_err());
    assert!(
        fixture.plan.backup.is_dir(),
        "replacement must have happened before the injected failure"
    );
    fixture.assert_original_restarted().await;
}

#[tokio::test]
async fn settings_write_failure_reuses_unchanged_original_settings() {
    let fixture = Fixture::new();
    let Some(_readonly) = ReadOnly::set(&fixture.plan.data_dir.join("config")) else {
        return;
    };
    assert!(fixture.activate().await.is_err());
    fixture.assert_original_restarted().await;
}

#[tokio::test]
async fn installation_failure_restarts_even_if_failure_reporting_also_fails() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.path().join("staging/ternilo-plugin")).unwrap();
    let Some(_readonly) = ReadOnly::set(&fixture.plan.data_dir.join("data/updates")) else {
        return;
    };
    assert!(fixture.activate().await.is_err());
    fixture.assert_original_restarted().await;
}

#[tokio::test]
async fn rollback_progress_failure_still_starts_and_checks_original_health() {
    let fixture = Fixture::new();
    let Some(_readonly) = ReadOnly::set(&fixture.plan.data_dir.join("data/updates")) else {
        return;
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let info = crate::service::ServiceInfo {
        service_id: "restored-fixture".into(),
        version: "1.0.0".into(),
        pid: std::process::id(),
        address: listener.local_addr().unwrap(),
        browser_enabled: true,
        remote_enabled: false,
    };
    let proof = fixture.root.path().join("restarted");
    let connection = fixture.plan.data_dir.join("runtime/service.json");
    let health = tokio::spawn(async move {
        while !proof.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        write_private(
            &connection,
            &crate::service::ServiceConnection {
                info: info.clone(),
                api_token: "fixture-token".into(),
            },
        )
        .unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
        }
        let body = serde_json::to_string(&info).unwrap();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        restart_restored(
            &fixture.plan,
            ClientUpdatePhase::RolledBack,
            "new executable failed".into(),
        ),
    )
    .await;
    if result.is_err() {
        health.abort();
    }
    assert!(
        result
            .expect("recovery must reach the original health check")
            .is_err()
    );
    fixture.assert_original_restarted().await;
    health.await.unwrap();
}

#[tokio::test]
async fn unverifiable_original_is_not_started_and_keeps_recovery_marker() {
    let fixture = Fixture::new();
    fs::write(
        fixture.plan.settings.installation.bin().join(executable()),
        "#!/bin/sh\nexit 1\n",
    )
    .unwrap();
    fs::remove_file(fixture.root.path().join("staging/ternilo-plugin")).unwrap();
    assert!(fixture.activate().await.is_err());
    assert!(!fixture.root.path().join("restarted").exists());
    assert!(
        fixture
            .plan
            .settings
            .installation
            .control_dir()
            .join(".ternilo-installing.json")
            .is_file()
    );
}

#[test]
fn recovery_replaces_invalid_settings_from_the_saved_plan() {
    let fixture = Fixture::new();
    fs::write(
        fixture.plan.data_dir.join("config/client-updates.json"),
        b"{invalid",
    )
    .unwrap();
    restore_installation(&fixture.plan, current_target().unwrap()).unwrap();
    assert!(
        load_settings(&fixture.plan.data_dir).unwrap().as_ref() == Some(&fixture.plan.settings)
    );
}
