//! Native launchd tests: the updater is born inside the agent, then survives bootout.
use super::*;
use crate::updates::{UpdateRequest, installation::Installation, launch_agent};
use serde::{Deserialize, Serialize};
use std::{os::unix::process::CommandExt, path::PathBuf};

const FIXTURE_TEST: &str = "updates::apply::launch_agent_tests::launch_agent_fixture";

#[derive(Serialize, Deserialize)]
struct Fixture {
    plan: ApplyPlan,
    staging: PathBuf,
    expected: String,
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(90), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("fixture did not produce {}", path.display()));
}

// Invoked by launchd and by its detached child; it does nothing in an ordinary test run.
#[tokio::test]
async fn launch_agent_fixture() {
    let Some(directory) = std::env::var_os("TERNILO_LA_FIXTURE") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let fixture: Fixture =
        serde_json::from_slice(&fs::read(directory.join("fixture.json")).unwrap()).unwrap();
    if std::env::var("TERNILO_LA_ROLE").as_deref() == Ok("helper") {
        fs::write(directory.join("helper.pid"), std::process::id().to_string()).unwrap();
        wait_for_file(&directory.join("activate")).await;
        let result = async {
            let _operation = operation_guard(&fixture.plan.data_dir)?;
            let service = crate::service::discover(&fixture.plan.data_dir)
                .await?
                .unwrap();
            verify_manager_process(&fixture.plan.settings.restart, service.info.pid).await?;
            activate(
                &fixture.plan,
                &fixture.staging,
                &fixture.expected,
                current_target().unwrap(),
            )
            .await
        }
        .await;
        write_private(
            &directory.join("result.json"),
            &result.map_err(|error| error.to_string()),
        )
        .unwrap();
        return;
    }
    if !directory.join("activate").exists() {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", FIXTURE_TEST, "--nocapture"])
            .env("TERNILO_LA_ROLE", "helper");
        detach(&mut command);
        command.spawn().unwrap();
    }
    let error =
        std::process::Command::new(fixture.plan.settings.installation.bin().join("ternilo"))
            .args(&fixture.plan.arguments)
            .exec();
    panic!("fixture client exec failed: {error}");
}

struct AgentGuard {
    target: String,
    directory: PathBuf,
}
impl Drop for AgentGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "{}",
                fs::read_to_string(self.directory.join("agent.log")).unwrap_or_default()
            );
        }
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["bootout", &self.target])
            .output();
        if !self.directory.join("result.json").exists()
            && let Ok(pid) = fs::read_to_string(self.directory.join("helper.pid"))
            && let Ok(pid) = pid.parse::<i32>()
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[tokio::test]
#[ignore = "requires a built client and an active macOS GUI login domain; exercised by native CI"]
#[expect(
    clippy::too_many_lines,
    reason = "Keep native agent setup, installation, rollback and cleanup assertions in one lifecycle scenario."
)]
async fn launch_agent_installs_and_rolls_back_with_keepalive() {
    let binary = std::env::var_os("TERNILO_LA_TEST_CLIENT").map_or_else(
        || {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("ternilo")
        },
        PathBuf::from,
    );
    assert!(
        binary.is_file(),
        "build ternilo first, or set TERNILO_LA_TEST_CLIENT to its path"
    );
    for rollback in [false, true] {
        let temporary = tempfile::Builder::new()
            .prefix("ternilo LaunchAgent & ")
            .tempdir()
            .unwrap();
        let directory = fs::canonicalize(temporary.path()).unwrap();
        let root = directory.join("data");
        ternilo_local::prepare_data_dir(&root).unwrap();
        let bin = directory.join("bin");
        let staging = directory.join("staging");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&staging).unwrap();
        for destination in [&bin, &staging] {
            fs::copy(&binary, destination.join("ternilo")).unwrap();
            fs::write(
                destination.join("ternilo-plugin"),
                if destination == &bin {
                    "previous plugin"
                } else {
                    "updated plugin"
                },
            )
            .unwrap();
        }
        fs::write(bin.join(".ternilo-update.lock"), "").unwrap();
        let label = format!(
            "com.ternilo.test.{}.{}",
            std::process::id(),
            u8::from(rollback)
        );
        let plist = directory.join("agent.plist");
        let test_executable = std::env::current_exe().unwrap();
        let session = if rollback {
            "<key>LimitLoadToSessionType</key><string>Background</string>"
        } else {
            ""
        };
        fs::write(&plist, format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{test}</string><string>--exact</string><string>{FIXTURE_TEST}</string><string>--nocapture</string></array>
<key>EnvironmentVariables</key><dict><key>TERNILO_LA_FIXTURE</key><string>{directory}</string><key>TERNILO_LA_ROLE</key><string>service</string></dict>
<key>KeepAlive</key><true/><key>RunAtLoad</key><true/>{session}
<key>StandardOutPath</key><string>{directory}/agent.log</string><key>StandardErrorPath</key><string>{directory}/agent.log</string>
</dict></plist>"#, test=xml(test_executable.to_str().unwrap()), directory=xml(directory.to_str().unwrap()))).unwrap();
        if rollback {
            assert!(
                std::process::Command::new("/usr/bin/plutil")
                    .arg("-convert")
                    .arg("binary1")
                    .arg(&plist)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let restart = launch_agent::configure(
            label.clone(),
            Some(plist.clone()),
            if rollback {
                Some(crate::updates::cli::LaunchdDomain::User)
            } else {
                None
            },
        )
        .await
        .unwrap();
        let RestartMethod::LaunchAgent { domain, .. } = &restart else {
            unreachable!()
        };
        let _guard = AgentGuard {
            target: format!("{domain}/{label}"),
            directory: directory.clone(),
        };
        let settings = UpdateSettings {
            installation: Installation::Flat { bin: bin.clone() },
            restart: restart.clone(),
        };
        write_private(&root.join("config/client-updates.json"), &settings).unwrap();
        write_private(&root.join("config/client-updates-enabled.json"), &true).unwrap();
        let previous_version = env!("CARGO_PKG_VERSION").to_owned();
        let expected = if rollback {
            "99.0.0".to_owned()
        } else {
            previous_version.clone()
        };
        let plan = ApplyPlan {
            request: UpdateRequest {
                id: "launchagent-native-fixture".into(),
                manifest: String::new(),
                signature: String::new(),
                server_version: expected.clone(),
                download_token: String::new(),
                expires_at_ms: u64::MAX,
            },
            settings,
            data_dir: root.clone(),
            archive: directory.join("unused.tar.gz"),
            service_id: String::new(),
            backup: directory.join("backup"),
            previous_version: previous_version.clone(),
            arguments: vec![
                "serve".into(),
                "--listen".into(),
                "127.0.0.1:0".into(),
                "--data-dir".into(),
                root.to_str().unwrap().into(),
            ],
        };
        write_private(
            &directory.join("fixture.json"),
            &Fixture {
                plan,
                staging,
                expected,
            },
        )
        .unwrap();
        write_private(
            &root.join("data/updates/progress.json"),
            &ternilo_protocol::ClientUpdateProgress {
                id: "launchagent-native-fixture".into(),
                version: previous_version.clone(),
                phase: ClientUpdatePhase::Installing,
                revision: 1,
                error: None,
            },
        )
        .unwrap();
        launch_agent::action(&label, domain, &plist, "start")
            .await
            .unwrap();
        wait_for_version(&root, &previous_version).await.unwrap();
        let service = crate::service::discover(&root).await.unwrap().unwrap();
        verify_manager_process(&restart, service.info.pid)
            .await
            .unwrap();
        assert!(
            verify_manager_process(&restart, service.info.pid + 1)
                .await
                .is_err()
        );
        assert!(
            launch_agent::verify("wrong.label", domain, &plist, service.info.pid)
                .await
                .is_err()
        );
        // Starting an already registered agent must not duplicate it or replace its PID.
        launch_agent::action(&label, domain, &plist, "start")
            .await
            .unwrap();
        assert_eq!(
            crate::service::discover(&root)
                .await
                .unwrap()
                .unwrap()
                .info
                .pid,
            service.info.pid
        );
        fs::write(directory.join("activate"), "go").unwrap();
        wait_for_file(&directory.join("result.json")).await;
        let outcome: Result<(), String> =
            serde_json::from_slice(&fs::read(directory.join("result.json")).unwrap()).unwrap();
        assert!(
            outcome.is_ok(),
            "{outcome:?}\n{}",
            fs::read_to_string(directory.join("agent.log")).unwrap_or_default()
        );
        wait_for_version(&root, &previous_version).await.unwrap();
        let after = crate::service::discover(&root).await.unwrap().unwrap();
        assert_ne!(after.info.pid, service.info.pid);
        verify_manager_process(&restart, after.info.pid)
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(bin.join("ternilo-plugin")).unwrap(),
            if rollback {
                "previous plugin"
            } else {
                "updated plugin"
            }
        );
        assert_eq!(
            read_progress(&root).unwrap().unwrap().phase,
            if rollback {
                ClientUpdatePhase::RolledBack
            } else {
                ClientUpdatePhase::AwaitingReconnect
            }
        );
        assert!(!bin.join(".ternilo-installing.json").exists());
        drop(operation_guard(&root).unwrap());
        launch_agent::action(&label, domain, &plist, "stop")
            .await
            .unwrap();
        assert_eq!(
            std::process::Command::new("/bin/launchctl")
                .args(["print", &format!("{domain}/{label}")])
                .output()
                .unwrap()
                .status
                .code(),
            Some(113),
            "stop must finish unregistering the job before it can be bootstrapped again",
        );
        tokio::time::timeout(Duration::from_secs(30), async {
            while crate::service::discover(&root).await.unwrap().is_some() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        // Recovery may stop an already booted-out job, then bootstrap its saved plist.
        launch_agent::action(&label, domain, &plist, "stop")
            .await
            .unwrap();
        start_saved_service(&root, &load_settings(&root).unwrap().unwrap())
            .await
            .unwrap();
        wait_for_version(&root, &previous_version).await.unwrap();
    }
}
