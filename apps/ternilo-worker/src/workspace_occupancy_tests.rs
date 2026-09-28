use std::fs;
use std::{path::PathBuf, process::Stdio, time::Duration};

use ternilo_cloud::CloudWorkerIdentity;
use ternilo_protocol::{TenantId, WorkspaceId};
use ternilo_transport::ExecutorId;

use super::*;

fn owner(worker: &str, generation: u64) -> OccupancyOwner {
    OccupancyOwner {
        worker: CloudWorkerIdentity {
            worker_id: ExecutorId::new(worker),
            instance_nonce: format!("{worker}-instance"),
            generation,
        },
        family_id: format!("family-{worker}"),
        occupation_epoch: 1,
        supervisor: SupervisorIdentity::current().unwrap(),
    }
}

fn root() -> (tempfile::TempDir, RegisteredStorageRoot) {
    let directory = tempfile::tempdir().unwrap();
    let root_id = crate::storage_root::load_or_create(directory.path(), "occupancy-test").unwrap();
    let root = RegisteredStorageRoot::open(directory.path(), "occupancy-test", &root_id).unwrap();
    (directory, root)
}

#[test]
fn dropped_guard_leaves_a_durable_uncertain_record_until_explicit_completion() {
    let (_directory, root) = root();
    let occupancy = WorkspaceOccupancy::open(
        &root,
        &TenantId::new("tenant"),
        &WorkspaceId::new("workspace"),
    )
    .unwrap();
    let first_owner = owner("worker-a", 1);
    let OccupancyAdmission::Acquired(guard) = occupancy.try_acquire(&first_owner, "run-a").unwrap()
    else {
        panic!("expected first acquisition")
    };
    drop(guard);

    let second = occupancy.try_acquire(&first_owner, "run-b").unwrap();
    assert!(matches!(second, OccupancyAdmission::Unconfirmed(_)));
    let other_worker = occupancy
        .try_acquire(&owner("worker-b", 1), "run-c")
        .unwrap();
    assert!(matches!(other_worker, OccupancyAdmission::Unconfirmed(_)));
}

#[test]
fn explicit_completion_allows_a_new_worker_only_after_the_member_is_removed() {
    let (_directory, root) = root();
    let occupancy = WorkspaceOccupancy::open(
        &root,
        &TenantId::new("tenant"),
        &WorkspaceId::new("workspace"),
    )
    .unwrap();
    let first_owner = owner("worker-a", 1);
    let OccupancyAdmission::Acquired(guard) = occupancy.try_acquire(&first_owner, "run-a").unwrap()
    else {
        panic!("expected first acquisition")
    };
    guard.confirm_stopped().unwrap();
    let mut second_owner = owner("worker-b", 1);
    second_owner.occupation_epoch = 2;
    assert!(matches!(
        occupancy.try_acquire(&second_owner, "run-b").unwrap(),
        OccupancyAdmission::Acquired(_)
    ));
}

#[test]
fn same_worker_generation_can_join_but_a_new_instance_cannot() {
    let (_directory, root) = root();
    let occupancy = WorkspaceOccupancy::open(
        &root,
        &TenantId::new("tenant"),
        &WorkspaceId::new("workspace"),
    )
    .unwrap();
    let first_owner = owner("worker-a", 7);
    let OccupancyAdmission::Acquired(guard) = occupancy.try_acquire(&first_owner, "run-a").unwrap()
    else {
        panic!("expected first acquisition")
    };
    // A second execution in the same worker generation can join while the first member lock is
    // held; another worker would receive Busy instead of sharing this live scope.
    let OccupancyAdmission::Acquired(second) =
        occupancy.try_acquire(&first_owner, "run-b").unwrap()
    else {
        panic!("expected same-generation acquisition")
    };
    second.confirm_stopped().unwrap();
    guard.confirm_stopped().unwrap();
}

#[test]
fn completed_tickets_and_older_epochs_cannot_be_replayed_after_reopening() {
    let (_directory, root) = root();
    let tenant = TenantId::new("tenant");
    let workspace = WorkspaceId::new("workspace");
    let occupancy = WorkspaceOccupancy::open(&root, &tenant, &workspace).unwrap();
    let original = owner("worker-a", 1);
    let OccupancyAdmission::Acquired(first) = occupancy.try_acquire(&original, "first").unwrap()
    else {
        panic!("expected initial acquisition");
    };
    first.confirm_stopped().unwrap();
    drop(occupancy);
    let reopened = WorkspaceOccupancy::open(&root, &tenant, &workspace).unwrap();
    assert!(reopened.try_acquire(&original, "first").is_err());
    // A member already admitted by Server can arrive after its sibling completed.
    let OccupancyAdmission::Acquired(late) =
        reopened.try_acquire(&original, "late-sibling").unwrap()
    else {
        panic!("expected late same-epoch member");
    };
    let mut successor = owner("worker-b", 1);
    successor.occupation_epoch = 2;
    assert!(matches!(
        reopened.try_acquire(&successor, "next").unwrap(),
        OccupancyAdmission::Busy
    ));
    late.confirm_stopped().unwrap();
    let OccupancyAdmission::Acquired(next) = reopened.try_acquire(&successor, "next").unwrap()
    else {
        panic!("expected next occupation");
    };
    next.confirm_stopped().unwrap();
    assert!(
        reopened
            .try_acquire(&original, "old-delayed-member")
            .is_err()
    );
    assert!(reopened.try_acquire(&successor, "next").is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn a_replaced_storage_root_is_rejected_before_occupancy_access() {
    let (directory, root) = root();
    let original = directory.path().to_owned();
    let backup = original.with_extension("old");
    fs::rename(&original, &backup).unwrap();
    std::os::unix::fs::symlink(&backup, &original).unwrap();
    assert!(
        WorkspaceOccupancy::open(
            &root,
            &TenantId::new("tenant"),
            &WorkspaceId::new("workspace"),
        )
        .is_err()
    );
    fs::remove_file(&original).unwrap();
    fs::rename(backup, directory.path()).unwrap();
}

#[cfg(unix)]
#[test]
fn control_directory_rejects_symlink_replacement() {
    let (_directory, root) = root();
    let occupancy = WorkspaceOccupancy::open(
        &root,
        &TenantId::new("tenant"),
        &WorkspaceId::new("workspace"),
    )
    .unwrap();
    let control = root.path().join(CONTROL_DIRECTORY);
    let outside = tempfile::tempdir().unwrap();
    let workspace_key = digest("tenant\0workspace");
    let workspace = control.join(workspace_key);
    fs::remove_dir(&workspace).unwrap();
    std::os::unix::fs::symlink(outside.path(), &workspace).unwrap();
    assert!(
        occupancy
            .try_acquire(&owner("worker-a", 1), "run-a")
            .is_err()
    );
}

#[test]
#[cfg(unix)]
fn a_second_worker_cannot_take_over_after_the_first_process_is_killed() {
    let (directory, root) = root();
    let ready = directory.path().join("holder-ready");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg("workspace_occupancy::tests::occupancy_holder_fixture")
        .arg("--ignored")
        .arg("--nocapture")
        .env("TERNILO_OCCUPANCY_ROOT", root.path())
        .env("TERNILO_OCCUPANCY_READY", &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut holder = command.spawn().unwrap();
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        ready.exists(),
        "the independent occupancy holder must acquire its member lock"
    );
    let occupancy = WorkspaceOccupancy::open(
        &root,
        &TenantId::new("tenant"),
        &WorkspaceId::new("workspace"),
    )
    .unwrap();
    assert!(matches!(
        occupancy
            .try_acquire(&owner("worker-b", 1), "run-b")
            .unwrap(),
        OccupancyAdmission::Busy
    ));
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(matches!(
        occupancy
            .try_acquire(&owner("worker-b", 1), "run-c")
            .unwrap(),
        OccupancyAdmission::Unconfirmed(_)
    ));
}

#[test]
#[ignore = "Subprocess entry point for cross-process occupancy lifecycle verification."]
fn occupancy_holder_fixture() {
    let root_path = PathBuf::from(std::env::var_os("TERNILO_OCCUPANCY_ROOT").unwrap());
    let ready = PathBuf::from(std::env::var_os("TERNILO_OCCUPANCY_READY").unwrap());
    let root = RegisteredStorageRoot::open(&root_path, "occupancy-test", &load_marker(&root_path))
        .unwrap();
    let occupancy = WorkspaceOccupancy::open(
        &root,
        &TenantId::new("tenant"),
        &WorkspaceId::new("workspace"),
    )
    .unwrap();
    let OccupancyAdmission::Acquired(guard) = occupancy
        .try_acquire(&owner("worker-a", 1), "run-a")
        .unwrap()
    else {
        panic!("the fixture must acquire the occupancy member")
    };
    fs::write(ready, b"ready").unwrap();
    std::thread::sleep(Duration::from_secs(10));
    drop(guard);
}

fn load_marker(root: &std::path::Path) -> String {
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(".ternilo-storage.json")).unwrap()).unwrap();
    value["root_id"].as_str().unwrap().to_owned()
}
