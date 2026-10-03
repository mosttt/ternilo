use std::{process::Stdio, time::Duration};

use ternilo_cloud::WorkspaceUseTicket;
use ternilo_protocol::RunId;
use ternilo_transport::ExecutorId;

use super::super::*;

fn fixture() -> (
    tempfile::TempDir,
    RegisteredStorageRoot,
    WorkspaceUseTicket,
    OccupancyOwner,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = RegisteredStorageRoot::initialize(directory.path(), "recovery-test", None).unwrap();
    let ticket = WorkspaceUseTicket {
        storage_id: root.storage_id().to_owned(),
        root_id: root.root_id().to_owned(),
        tenant_id: TenantId::new("tenant"),
        workspace_id: WorkspaceId::new("workspace"),
        family_id: "recovery-family".to_owned(),
        worker_id: "worker-a".to_owned(),
        worker_generation: 1,
        occupation_epoch: 1,
        run_id: RunId::new("recover-run"),
        lease_token: 1,
    };
    let owner = OccupancyOwner {
        worker: CloudWorkerIdentity {
            worker_id: ExecutorId::new("worker-a"),
            instance_nonce: "original".to_owned(),
            generation: 1,
        },
        family_id: ticket.family_id.clone(),
        occupation_epoch: 1,
        supervisor: SupervisorIdentity::current().unwrap(),
    };
    (directory, root, ticket, owner)
}

#[test]
fn vanished_member_lock_cannot_release_a_still_running_supervisor() {
    let (_directory, root, ticket, owner) = fixture();
    let occupancy =
        WorkspaceOccupancy::open(&root, &ticket.tenant_id, &ticket.workspace_id).unwrap();
    let OccupancyAdmission::Acquired(guard) =
        occupancy.try_acquire(&owner, "recover-run:1").unwrap()
    else {
        panic!("expected acquisition");
    };
    assert!(!WorkspaceOccupancy::recover(&root, &ticket).unwrap());
    drop(guard);
    assert!(!WorkspaceOccupancy::recover(&root, &ticket).unwrap());
    assert!(
        occupancy
            .inner
            .read_record()
            .unwrap()
            .unwrap()
            .executions
            .contains("recover-run:1")
    );
}

#[test]
fn completed_marker_retries_confirmation_but_never_releases_a_new_epoch() {
    let (_directory, root, ticket, owner) = fixture();
    let occupancy =
        WorkspaceOccupancy::open(&root, &ticket.tenant_id, &ticket.workspace_id).unwrap();
    let OccupancyAdmission::Acquired(guard) =
        occupancy.try_acquire(&owner, "recover-run:1").unwrap()
    else {
        panic!("expected acquisition");
    };
    guard.confirm_stopped().unwrap();
    assert!(WorkspaceOccupancy::recover(&root, &ticket).unwrap());
    assert!(WorkspaceOccupancy::recover(&root, &ticket).unwrap());
    let mut next = owner;
    next.occupation_epoch = 2;
    let OccupancyAdmission::Acquired(guard) = occupancy.try_acquire(&next, "next-run:1").unwrap()
    else {
        panic!("expected next acquisition");
    };
    assert!(!WorkspaceOccupancy::recover(&root, &ticket).unwrap());
    assert!(
        occupancy
            .inner
            .read_record()
            .unwrap()
            .unwrap()
            .executions
            .contains("next-run:1")
    );
    guard.confirm_stopped().unwrap();
}

#[test]
fn recovery_requires_the_original_storage_and_recorded_ticket() {
    let (_directory, root, ticket, owner) = fixture();
    assert!(!WorkspaceOccupancy::recover(&root, &ticket).unwrap());
    let occupancy =
        WorkspaceOccupancy::open(&root, &ticket.tenant_id, &ticket.workspace_id).unwrap();
    let OccupancyAdmission::Acquired(guard) =
        occupancy.try_acquire(&owner, "recover-run:1").unwrap()
    else {
        panic!("expected acquisition");
    };
    guard.confirm_stopped().unwrap();
    let mut wrong = ticket.clone();
    "foreign".clone_into(&mut wrong.storage_id);
    assert!(WorkspaceOccupancy::recover(&root, &wrong).is_err());
    let mut wrong = ticket.clone();
    wrong.worker_generation += 1;
    assert!(!WorkspaceOccupancy::recover(&root, &wrong).unwrap());
    let mut wrong = ticket;
    wrong.lease_token += 1;
    assert!(!WorkspaceOccupancy::recover(&root, &wrong).unwrap());
}

#[tokio::test]
async fn dead_supervisor_recovers_only_unstarted_or_verified_executions() {
    for authorized in [false, true] {
        let (directory, root, ticket, _) = fixture();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "workspace_occupancy::recovery::tests::supervisor_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("TERNILO_RECOVERY_ROOT", root.path())
            .env(
                "TERNILO_RECOVERY_AUTHORIZED",
                if authorized { "yes" } else { "no" },
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !directory.path().join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(!WorkspaceOccupancy::recover(&root, &ticket).unwrap());
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        let occupancy =
            WorkspaceOccupancy::open(&root, &ticket.tenant_id, &ticket.workspace_id).unwrap();
        let saved = occupancy.inner.read_record().unwrap().unwrap();
        for foreign_boot in [true, false] {
            let mut foreign = saved.clone();
            if foreign_boot {
                foreign.owner.supervisor.boot_id.push_str("-foreign");
            } else {
                foreign.owner.supervisor.observer_namespace += 1;
            }
            occupancy.inner.write_record(&foreign).unwrap();
            assert!(!WorkspaceOccupancy::recover(&root, &ticket).unwrap());
        }
        occupancy.inner.write_record(&saved).unwrap();
        assert_eq!(
            WorkspaceOccupancy::recover(&root, &ticket).unwrap(),
            !authorized,
            "an authorized process without a namespace exit identity remains held"
        );
        assert_eq!(
            WorkspaceOccupancy::recover(&root, &ticket).unwrap(),
            !authorized
        );
    }
}

#[tokio::test]
#[ignore = "subprocess fixture invoked by the recovery contract"]
async fn supervisor_fixture() {
    let path = std::env::var("TERNILO_RECOVERY_ROOT").unwrap();
    let root =
        RegisteredStorageRoot::initialize(std::path::Path::new(&path), "recovery-test", None)
            .unwrap();
    let occupancy = WorkspaceOccupancy::open(
        &root,
        &TenantId::new("tenant"),
        &WorkspaceId::new("workspace"),
    )
    .unwrap();
    let owner = OccupancyOwner {
        worker: CloudWorkerIdentity {
            worker_id: ExecutorId::new("worker-a"),
            instance_nonce: "original".to_owned(),
            generation: 1,
        },
        family_id: "recovery-family".to_owned(),
        occupation_epoch: 1,
        supervisor: SupervisorIdentity::current().unwrap(),
    };
    let OccupancyAdmission::Acquired(guard) =
        occupancy.try_acquire(&owner, "recover-run:1").unwrap()
    else {
        panic!("expected acquisition");
    };
    let mode = std::env::var("TERNILO_RECOVERY_AUTHORIZED").unwrap();
    let _sandbox = if mode == "namespace" {
        Some(orphan_sandbox(&root, &guard).await)
    } else {
        if mode == "yes" {
            guard.authorize_startup().unwrap();
        }
        None
    };
    std::fs::write(root.path().join("ready"), b"ready").unwrap();
    std::thread::sleep(Duration::from_secs(60));
    drop(guard);
}

#[test]
fn metadata_gate_ends_even_if_a_fork_inherits_its_open_file_description() {
    let (_directory, root, ticket, _) = fixture();
    let occupancy =
        WorkspaceOccupancy::open(&root, &ticket.tenant_id, &ticket.workspace_id).unwrap();
    let gate = occupancy.inner.gate().unwrap();
    let inherited = gate.0.try_clone().unwrap();
    assert!(occupancy.inner.try_gate().unwrap().is_none());
    drop(gate);
    assert!(
        occupancy.inner.try_gate().unwrap().is_some(),
        "an inherited descriptor must not outlive the completed metadata transaction"
    );
    drop(inherited);
}

async fn orphan_sandbox(
    root: &RegisteredStorageRoot,
    guard: &OccupancyGuard,
) -> crate::process_lifetime::ManagedChild {
    use crate::{process_lifetime::ManagedChild, sandbox_lifetime::SandboxLifetime};
    let mut command = tokio::process::Command::new("/usr/bin/bwrap");
    SandboxLifetime::add_options(&mut command);
    // Deliberately omit die-with-parent to exercise explicit cleanup of a surviving init.
    command.args(["--unshare-all", "--new-session", "--ro-bind", "/usr", "/usr",
        "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64",
        "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--bind"])
        .arg(root.path()).arg("/workspace").args(["--chdir", "/workspace", "--", "/bin/sh", "-c",
            "setsid sh -c 'while :; do printf x >> /workspace/writes; sleep 0.01; done' & writer=$!; sleep 30; kill $writer; wait"])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit());
    let lifetime = SandboxLifetime::prepare(&mut command).unwrap();
    let mut child = ManagedChild::spawn(&mut command, false, Some(lifetime)).unwrap();
    drop(command);
    child
        .establish_sandbox(|identity| {
            guard
                .record_namespace(identity)
                .map_err(std::io::Error::other)?;
            guard.authorize_startup().map_err(std::io::Error::other)
        })
        .await
        .unwrap();
    child
}

#[tokio::test]
async fn recovery_terminates_the_verified_orphan_namespace_before_releasing_its_member() {
    let (_directory, root, ticket, _) = fixture();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "workspace_occupancy::recovery::tests::supervisor_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("TERNILO_RECOVERY_ROOT", root.path())
        .env("TERNILO_RECOVERY_AUTHORIZED", "namespace")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let writes = root.path().join("writes");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("ready").exists()
            || !std::fs::metadata(&writes).is_ok_and(|file| file.len() > 2)
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!WorkspaceOccupancy::recover(&root, &ticket).unwrap());
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let previous = std::fs::metadata(&writes).unwrap().len();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(
        std::fs::metadata(&writes).unwrap().len() > previous,
        "the fixture namespace must actually survive its original supervisor"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !WorkspaceOccupancy::recover(&root, &ticket).unwrap() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let stopped = std::fs::metadata(&writes).unwrap().len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(std::fs::metadata(&writes).unwrap().len(), stopped);
    assert!(WorkspaceOccupancy::recover(&root, &ticket).unwrap());
}
