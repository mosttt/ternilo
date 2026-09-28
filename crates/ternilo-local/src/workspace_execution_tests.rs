use std::{io::Read, process::Stdio};

use tokio::process::{Child, Command};

use super::*;

struct Fixture {
    directory: tempfile::TempDir,
    root: PathBuf,
    workspace: PathBuf,
    left: PathBuf,
    right: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("locks");
        let workspace = directory.path().join("workspace");
        let left = workspace.join("left");
        let right = workspace.join("right");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        Self {
            directory,
            root,
            workspace,
            left,
            right,
        }
    }

    fn bind(&self, scope: &str, workspace: &Path) -> Arc<dyn WorkspaceExecution> {
        DirectoryCoordinator::new(self.root.clone()).bind(scope.to_owned(), workspace.to_owned())
    }

    fn bind_user(&self, user: &str, workspace: &Path) -> Arc<dyn WorkspaceExecution> {
        DirectoryCoordinator::new(self.root.clone())
            .bind_user(user.to_owned(), workspace.to_owned())
    }

    fn lock_registry(&self) -> AcquiredFileLock {
        create_lock_root(&self.root).unwrap();
        let gate = private_file_options()
            .create(true)
            .truncate(false)
            .open(self.root.join("registry-gate.lock"))
            .unwrap();
        try_lock(gate).unwrap().unwrap()
    }
}

#[tokio::test]
async fn one_coordinator_reenters_until_the_final_lease_is_dropped() {
    let fixture = Fixture::new();
    let coordinator = DirectoryCoordinator::new(fixture.root.clone());
    let first_binding = coordinator.bind("family".to_owned(), fixture.left.clone());
    let second_binding = coordinator
        .clone()
        .bind("family".to_owned(), fixture.left.clone());
    let first = first_binding.try_acquire().await.unwrap().unwrap();
    let second = second_binding.try_acquire().await.unwrap().unwrap();
    let cloned = first.clone();
    let independent = fixture.bind("family", &fixture.left);
    let unrelated_scope = coordinator.bind("another-family".to_owned(), fixture.left.clone());
    assert!(independent.try_acquire().await.unwrap().is_none());
    assert!(unrelated_scope.try_acquire().await.unwrap().is_none());
    drop(first);
    drop(second);
    assert!(independent.try_acquire().await.unwrap().is_none());
    drop(cloned);
    assert!(independent.try_acquire().await.unwrap().is_some());
    assert!(std::fs::read_dir(&fixture.left).unwrap().next().is_none());
}

#[tokio::test]
async fn duplicated_descriptors_do_not_extend_the_final_logical_lease() {
    let fixture = Fixture::new();
    let coordinator = DirectoryCoordinator::new(fixture.root.clone());
    let binding = coordinator.bind("family".to_owned(), fixture.left.clone());
    let first = coordinator
        .try_acquire(
            &DirectoryOwner::Family {
                instance: coordinator.state.instance.clone(),
                scope: "family".to_owned(),
            },
            &fixture.left,
        )
        .await
        .unwrap()
        .unwrap();
    let second = binding.try_acquire().await.unwrap().unwrap();
    let duplicated = first.0.try_clone().unwrap();
    let unrelated = fixture.bind("unrelated", &fixture.left);
    let parent = fixture.bind("parent", &fixture.workspace);
    drop(first);
    assert!(
        unrelated.try_acquire().await.unwrap().is_none(),
        "another logical lease still owns the directory"
    );
    assert!(parent.try_acquire().await.unwrap().is_none());
    drop(second);
    let next = parent
        .try_acquire()
        .await
        .unwrap()
        .expect("descriptor copies must not retain released directory ownership");
    assert!(
        unrelated.try_acquire().await.unwrap().is_none(),
        "the newly admitted parent remains exclusive"
    );
    drop(next);
    assert!(
        unrelated.try_acquire().await.unwrap().is_some(),
        "descriptor copies must not retain the released leaf lock"
    );
    assert!(
        duplicated.metadata().is_ok(),
        "the copied descriptors remain open throughout the assertions"
    );
}

#[tokio::test]
async fn parent_and_child_exclude_each_other_but_siblings_can_run_together() {
    let fixture = Fixture::new();
    let parent = fixture.bind("parent", &fixture.workspace);
    let left = fixture.bind("left", &fixture.left);
    let right = fixture.bind("right", &fixture.right);
    let parent_lease = parent.try_acquire().await.unwrap().unwrap();
    assert!(left.try_acquire().await.unwrap().is_none());
    assert!(right.try_acquire().await.unwrap().is_none());
    drop(parent_lease);
    let left_lease = left.try_acquire().await.unwrap().unwrap();
    let right_lease = right.try_acquire().await.unwrap().unwrap();
    assert!(parent.try_acquire().await.unwrap().is_none());
    drop(left_lease);
    assert!(parent.try_acquire().await.unwrap().is_none());
    drop(right_lease);
    assert!(parent.try_acquire().await.unwrap().is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_and_renamed_directory_keep_the_same_physical_ownership() {
    let fixture = Fixture::new();
    let alias = fixture.workspace.join("alias");
    std::os::unix::fs::symlink(&fixture.left, &alias).unwrap();
    let coordinator = DirectoryCoordinator::new(fixture.root.clone());
    let lease = coordinator
        .bind("family".to_owned(), fixture.left.clone())
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let aliased = coordinator
        .bind("family".to_owned(), alias.clone())
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .bind("family", &alias)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    drop(aliased);
    let renamed = fixture.workspace.join("renamed");
    std::fs::rename(&fixture.left, &renamed).unwrap();
    assert!(
        fixture
            .bind("family", &renamed)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    drop(lease);
    assert!(
        fixture
            .bind("family", &renamed)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn cancelled_wait_does_not_retain_partial_ancestor_locks() {
    let fixture = Fixture::new();
    let holder = fixture
        .bind("holder", &fixture.left)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let blocked = fixture.bind("waiting", &fixture.left);
    let cancellation = RunCancellation::new();
    let wait_cancellation = cancellation.clone();
    let waiting = tokio::spawn(async move { blocked.acquire(wait_cancellation).await });
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!waiting.is_finished());
    cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::Cancelled);
    drop(holder);
    assert!(
        fixture
            .bind("parent", &fixture.workspace)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_waiting_execution_acquires_after_the_previous_owner_releases() {
    let fixture = Fixture::new();
    let holder = fixture
        .bind("holder", &fixture.left)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let blocked = fixture.bind("waiting", &fixture.left);
    let waiting = tokio::spawn(async move { blocked.acquire(RunCancellation::new()).await });
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!waiting.is_finished());
    drop(holder);
    let lease = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .bind("third", &fixture.left)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    drop(lease);
    assert!(
        fixture
            .bind("third", &fixture.left)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn user_leases_overlap_across_coordinators_until_the_last_holder_releases() {
    let fixture = Fixture::new();
    let first = fixture
        .bind_user("alice", &fixture.left)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let second = fixture
        .bind_user("alice", &fixture.left)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let parent = fixture
        .bind_user("alice", &fixture.workspace)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let cloned = second.clone();
    let other_parent = fixture.bind_user("bob", &fixture.workspace);
    let other_leaf = fixture.bind_user("bob", &fixture.left);
    let other_sibling = fixture.bind_user("bob", &fixture.right);
    assert!(other_parent.try_acquire().await.unwrap().is_none());
    assert!(other_leaf.try_acquire().await.unwrap().is_none());
    assert!(other_sibling.try_acquire().await.unwrap().is_none());
    drop(parent);
    let sibling = other_sibling.try_acquire().await.unwrap().unwrap();
    drop(first);
    drop(second);
    assert!(other_leaf.try_acquire().await.unwrap().is_none());
    assert!(other_parent.try_acquire().await.unwrap().is_none());
    drop(cloned);
    let leaf = other_leaf.try_acquire().await.unwrap().unwrap();
    let parent = other_parent.try_acquire().await.unwrap().unwrap();
    assert!(
        fixture
            .bind_user("alice", &fixture.workspace)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    drop((leaf, parent, sibling));
    assert!(
        fixture
            .bind_user("alice", &fixture.workspace)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn legacy_families_do_not_reenter_ancestors_or_bypass_user_leases() {
    let fixture = Fixture::new();
    let coordinator = DirectoryCoordinator::new(fixture.root.clone());
    let family = coordinator.bind("alice".to_owned(), fixture.left.clone());
    let family_parent = coordinator.bind("alice".to_owned(), fixture.workspace.clone());
    let user = coordinator.bind_user("alice".to_owned(), fixture.left.clone());
    let user_parent = coordinator.bind_user("alice".to_owned(), fixture.workspace.clone());
    let held = family.try_acquire().await.unwrap().unwrap();
    assert!(family_parent.try_acquire().await.unwrap().is_none());
    assert!(user.try_acquire().await.unwrap().is_none());
    assert!(user_parent.try_acquire().await.unwrap().is_none());
    assert!(
        fixture
            .bind_user("alice", &fixture.right)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
    drop(held);
    let held = user_parent.try_acquire().await.unwrap().unwrap();
    assert!(family.try_acquire().await.unwrap().is_none());
    assert!(family_parent.try_acquire().await.unwrap().is_none());
    drop(held);
    let held = family_parent.try_acquire().await.unwrap().unwrap();
    assert!(family.try_acquire().await.unwrap().is_none());
    assert!(user.try_acquire().await.unwrap().is_none());
    drop(held);
    assert!(family.try_acquire().await.unwrap().is_some());
}

#[tokio::test]
async fn computer_owners_can_share_with_local_actions_without_merging_distinct_accounts() {
    let fixture = Fixture::new();
    let first = DirectoryCoordinator::new(fixture.root.clone())
        .bind_computer_owner("account:alice".to_owned(), fixture.left.clone());
    let second = DirectoryCoordinator::new(fixture.root.clone())
        .bind_computer_owner("account:bob".to_owned(), fixture.left.clone());
    let local = fixture.bind_user("local-user", &fixture.left);
    let alice = first.try_acquire().await.unwrap().unwrap();
    let local_lease = local.try_acquire().await.unwrap().unwrap();
    assert!(second.try_acquire().await.unwrap().is_none());
    let same_account = fixture.bind_user("account:alice", &fixture.left);
    drop(local_lease);
    let same_account_lease = same_account.try_acquire().await.unwrap().unwrap();
    drop(alice);
    assert!(second.try_acquire().await.unwrap().is_none());
    drop(same_account_lease);
    let local_lease = local.try_acquire().await.unwrap().unwrap();
    let bob = second.try_acquire().await.unwrap().unwrap();
    assert!(first.try_acquire().await.unwrap().is_none());
    drop(bob);
    let alice = first.try_acquire().await.unwrap().unwrap();
    assert!(second.try_acquire().await.unwrap().is_none());
    drop((alice, local_lease));
    assert!(second.try_acquire().await.unwrap().is_some());
}

#[tokio::test]
async fn cancelling_an_execution_does_not_release_other_user_holders() {
    let fixture = Fixture::new();
    let survivor = fixture
        .bind_user("alice", &fixture.left)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let binding = fixture.bind_user("alice", &fixture.workspace);
    let cancellation = RunCancellation::new();
    let execution_cancellation = cancellation.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let execution = tokio::spawn(async move {
        let held = binding
            .acquire(execution_cancellation.clone())
            .await
            .unwrap();
        started.send(()).unwrap();
        execution_cancellation.cancelled().await;
        drop(held);
    });
    tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .unwrap()
        .unwrap();
    let other = fixture.bind_user("bob", &fixture.left);
    assert!(other.try_acquire().await.unwrap().is_none());
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(2), execution)
        .await
        .unwrap()
        .unwrap();
    assert!(other.try_acquire().await.unwrap().is_none());
    assert!(
        fixture
            .bind_user("bob", &fixture.right)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
    drop(survivor);
    assert!(other.try_acquire().await.unwrap().is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn user_identity_respects_symlinks_and_renamed_physical_directories() {
    let fixture = Fixture::new();
    let alias = fixture.workspace.join("alias");
    std::os::unix::fs::symlink(&fixture.left, &alias).unwrap();
    let first = fixture
        .bind_user("alice", &fixture.left)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let second = fixture
        .bind_user("alice", &alias)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .bind_user("bob", &alias)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    let renamed = fixture.workspace.join("renamed");
    std::fs::rename(&fixture.left, &renamed).unwrap();
    let third = fixture
        .bind_user("alice", &renamed)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .bind_user("bob", &renamed)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    drop((first, second));
    assert!(
        fixture
            .bind_user("bob", &fixture.workspace)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    drop(third);
    assert!(
        fixture
            .bind_user("bob", &renamed)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
}

#[test]
#[ignore = "Subprocess entry point for directory admission tests."]
fn directory_lock_child_fixture() {
    let result = PathBuf::from(std::env::var_os("TERNILO_TEST_DIRECTORY_RESULT").unwrap());
    if let Some(release) = std::env::var_os("TERNILO_TEST_DIRECTORY_INHERITED_RELEASE") {
        std::fs::write(&result, "acquired").unwrap();
        while !Path::new(&release).exists() {
            std::thread::sleep(Duration::from_millis(5));
        }
        return;
    }
    let root = std::env::var_os("TERNILO_TEST_DIRECTORY_LOCK_ROOT").unwrap();
    let workspace = std::env::var_os("TERNILO_TEST_DIRECTORY_WORKSPACE").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let coordinator = DirectoryCoordinator::new(PathBuf::from(root));
    let binding = match std::env::var("TERNILO_TEST_DIRECTORY_USER") {
        Ok(user) => coordinator.bind_user(user, PathBuf::from(workspace)),
        Err(_) => coordinator.bind("same-public-scope".to_owned(), PathBuf::from(workspace)),
    };
    if let Some(start) = std::env::var_os("TERNILO_TEST_DIRECTORY_START") {
        std::fs::write(result.with_extension("ready"), "ready").unwrap();
        while !Path::new(&start).exists() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let lease = runtime.block_on(async {
        let mut admission = binding.try_acquire();
        let mut first_poll = true;
        let (lease, ()) = tokio::join!(
            async {
                tokio::time::timeout(
                    Duration::from_secs(5),
                    std::future::poll_fn(|context| {
                        let state = admission.as_mut().poll(context);
                        if first_poll {
                            std::fs::write(result.with_extension("attempted"), "attempted")
                                .unwrap();
                            first_poll = false;
                        }
                        state
                    }),
                )
                .await
                .expect("directory admission did not finish")
                .unwrap()
            },
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                std::fs::write(result.with_extension("heartbeat"), "heartbeat").unwrap();
            },
        );
        lease
    });
    std::fs::write(
        result,
        if lease.is_some() {
            "acquired"
        } else {
            "blocked"
        },
    )
    .unwrap();
    if lease.is_some() {
        let _ = std::io::stdin().read(&mut [0_u8]);
    }
    drop(lease);
}

async fn spawn_holder(fixture: &Fixture, workspace: &Path, name: &str) -> (Child, bool) {
    let (child, result) = start_holder(fixture, workspace, name, None, None);
    (child, holder_result(&result).await)
}

fn start_holder(
    fixture: &Fixture,
    workspace: &Path,
    name: &str,
    user: Option<&str>,
    start: Option<&Path>,
) -> (Child, PathBuf) {
    let result = fixture.directory.path().join(format!("{name}-result"));
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg("workspace_execution::tests::directory_lock_child_fixture")
        .arg("--ignored")
        .env("TERNILO_TEST_DIRECTORY_LOCK_ROOT", &fixture.root)
        .env("TERNILO_TEST_DIRECTORY_WORKSPACE", workspace)
        .env("TERNILO_TEST_DIRECTORY_RESULT", &result)
        .env_remove("TERNILO_TEST_DIRECTORY_USER")
        .env_remove("TERNILO_TEST_DIRECTORY_START")
        .env_remove("TERNILO_TEST_DIRECTORY_INHERITED_RELEASE")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(user) = user {
        command.env("TERNILO_TEST_DIRECTORY_USER", user);
    }
    if let Some(start) = start {
        command.env("TERNILO_TEST_DIRECTORY_START", start);
    }
    (command.spawn().unwrap(), result)
}

async fn holder_result(result: &Path) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match std::fs::read_to_string(result).as_deref() {
                Ok("acquired") => break true,
                Ok("blocked") => break false,
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("directory admission subprocess did not report its result")
}

async fn start_together(start: &Path, results: &[&Path]) {
    holder_stage(results, "ready").await;
    std::fs::write(start, "start").unwrap();
}

async fn holder_stage(results: &[&Path], stage: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while results
            .iter()
            .any(|result| !result.with_extension(stage).exists())
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("directory admission subprocesses did not reach the requested stage");
}

async fn release(mut child: Child) {
    drop(child.stdin.take());
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn real_processes_coordinate_directories_and_release_after_exit_or_kill() {
    let fixture = Fixture::new();
    let (holder, acquired) = spawn_holder(&fixture, &fixture.left, "holder").await;
    assert!(acquired);
    let same_scope = fixture.bind("same-public-scope", &fixture.left);
    assert!(same_scope.try_acquire().await.unwrap().is_none());
    let (same_directory, acquired) = spawn_holder(&fixture, &fixture.left, "same").await;
    assert!(!acquired);
    release(same_directory).await;
    #[cfg(unix)]
    {
        let alias = fixture.workspace.join("alias");
        std::os::unix::fs::symlink(&fixture.left, &alias).unwrap();
        let (aliased, acquired) = spawn_holder(&fixture, &alias, "aliased").await;
        assert!(!acquired);
        release(aliased).await;
    }
    let (parent, acquired) = spawn_holder(&fixture, &fixture.workspace, "parent").await;
    assert!(!acquired);
    release(parent).await;
    let (mut sibling, acquired) = spawn_holder(&fixture, &fixture.right, "sibling").await;
    assert!(acquired);
    release(holder).await;
    let released = same_scope.try_acquire().await.unwrap().unwrap();
    drop(released);
    sibling.kill().await.unwrap();
    assert!(!sibling.wait().await.unwrap().success());
    assert!(
        fixture
            .bind("after-kill", &fixture.right)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        fixture
            .bind("after-both", &fixture.workspace)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn concurrent_user_processes_share_overlaps_and_keep_independent_lifetimes() {
    let fixture = Fixture::new();
    let start = fixture.directory.path().join("start");
    #[cfg(unix)]
    let alias = {
        let alias = fixture.workspace.join("alias");
        std::os::unix::fs::symlink(&fixture.left, &alias).unwrap();
        alias
    };
    #[cfg(not(unix))]
    let alias = fixture.left.clone();
    let (mut first, first_result) = start_holder(
        &fixture,
        &fixture.left,
        "first",
        Some("alice"),
        Some(&start),
    );
    let (parent, parent_result) = start_holder(
        &fixture,
        &fixture.workspace,
        "parent",
        Some("alice"),
        Some(&start),
    );
    let (mut aliased, alias_result) =
        start_holder(&fixture, &alias, "alias", Some("alice"), Some(&start));
    start_together(&start, &[&first_result, &parent_result, &alias_result]).await;
    assert!(holder_result(&first_result).await);
    assert!(holder_result(&parent_result).await);
    assert!(holder_result(&alias_result).await);
    assert!(
        fixture
            .bind_user("bob", &fixture.workspace)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .bind_user("bob", &fixture.right)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    release(parent).await;
    let (sibling, sibling_result) =
        start_holder(&fixture, &fixture.right, "sibling", Some("bob"), None);
    assert!(holder_result(&sibling_result).await);
    first.kill().await.unwrap();
    assert!(!first.wait().await.unwrap().success());
    assert!(
        fixture
            .bind_user("bob", &fixture.left)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .bind("alice", &fixture.left)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    aliased.kill().await.unwrap();
    assert!(!aliased.wait().await.unwrap().success());
    let (next, next_result) = start_holder(&fixture, &fixture.left, "next", Some("bob"), None);
    assert!(holder_result(&next_result).await);
    assert!(
        fixture
            .bind_user("alice", &fixture.workspace)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    release(next).await;
    assert!(
        fixture
            .bind_user("alice", &fixture.workspace)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    release(sibling).await;
    assert!(
        fixture
            .bind_user("alice", &fixture.workspace)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn concurrent_processes_do_not_report_registry_contention_as_directory_contention() {
    let fixture = Fixture::new();
    let gate = fixture.lock_registry();
    let start = fixture.directory.path().join("start");
    let (left, left_result) =
        start_holder(&fixture, &fixture.left, "left", Some("alice"), Some(&start));
    let (parent, parent_result) = start_holder(
        &fixture,
        &fixture.workspace,
        "parent",
        Some("alice"),
        Some(&start),
    );
    let results = [left_result.as_path(), parent_result.as_path()];
    start_together(&start, &results).await;
    holder_stage(&results, "attempted").await;
    holder_stage(&results, "heartbeat").await;
    assert!(results.iter().all(|result| !result.exists()));
    drop(gate);
    assert!(holder_result(&left_result).await);
    assert!(holder_result(&parent_result).await);
    let blocked = fixture.bind_user("bob", &fixture.left);
    assert!(blocked.try_acquire().await.unwrap().is_none());
    release(left).await;
    assert!(blocked.try_acquire().await.unwrap().is_none());
    release(parent).await;
    assert!(blocked.try_acquire().await.unwrap().is_some());
}

#[tokio::test]
async fn registry_waits_can_be_cancelled_or_dropped_without_admitting_an_execution() {
    let fixture = Fixture::new();
    let survivor = fixture
        .bind_user("alice", &fixture.left)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let gate = fixture.lock_registry();
    let blocked = fixture.bind_user("bob", &fixture.workspace);
    let cancellation = RunCancellation::new();
    let mut waiting = blocked.acquire(cancellation.clone());
    std::future::poll_fn(|context| {
        assert!(waiting.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::Cancelled);
    let mut abandoned = blocked.try_acquire();
    std::future::poll_fn(|context| {
        assert!(abandoned.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(abandoned);
    drop(gate);
    assert!(blocked.try_acquire().await.unwrap().is_none());
    drop(survivor);
    assert!(blocked.try_acquire().await.unwrap().is_some());
}

#[tokio::test]
async fn competing_processes_never_admit_conflicting_owners_together() {
    for (left_user, parent_user) in [
        (Some("alice"), Some("bob")),
        (None, Some("same-public-scope")),
        (None, None),
    ] {
        let fixture = Fixture::new();
        let start = fixture.directory.path().join("start");
        let (left, left_result) =
            start_holder(&fixture, &fixture.left, "left", left_user, Some(&start));
        let (parent, parent_result) = start_holder(
            &fixture,
            &fixture.workspace,
            "parent",
            parent_user,
            Some(&start),
        );
        start_together(&start, &[&left_result, &parent_result]).await;
        let left_acquired = holder_result(&left_result).await;
        let parent_acquired = holder_result(&parent_result).await;
        assert_ne!(
            left_acquired, parent_acquired,
            "exactly one overlapping owner must enter"
        );
        assert!(
            fixture
                .bind_user("observer", &fixture.left)
                .try_acquire()
                .await
                .unwrap()
                .is_none()
        );
        release(left).await;
        if parent_acquired {
            assert!(
                fixture
                    .bind_user("observer", &fixture.left)
                    .try_acquire()
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        release(parent).await;
        let (next, result) =
            start_holder(&fixture, &fixture.workspace, "next", Some("observer"), None);
        assert!(holder_result(&result).await);
        release(next).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn inherited_descriptors_do_not_outlive_user_lease_release() {
    let fixture = Fixture::new();
    let coordinator = DirectoryCoordinator::new(fixture.root.clone());
    let first = coordinator
        .try_acquire(
            &DirectoryOwner::User {
                user: "alice".to_owned(),
                local_owner: false,
            },
            &fixture.left,
        )
        .await
        .unwrap()
        .unwrap();
    let second = fixture
        .bind_user("alice", &fixture.left)
        .try_acquire()
        .await
        .unwrap()
        .unwrap();
    let result = fixture.directory.path().join("inherited-result");
    let release_path = fixture.directory.path().join("release-inherited");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("workspace_execution::tests::directory_lock_child_fixture")
        .arg("--ignored")
        .env("TERNILO_TEST_DIRECTORY_RESULT", &result)
        .env("TERNILO_TEST_DIRECTORY_INHERITED_RELEASE", &release_path)
        .stdin(Stdio::from(first.0.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert!(holder_result(&result).await);
    let other = fixture.bind_user("bob", &fixture.workspace);
    drop(first);
    assert!(other.try_acquire().await.unwrap().is_none());
    drop(second);
    let next = other.try_acquire().await.unwrap().unwrap();
    assert!(child.try_wait().unwrap().is_none());
    assert!(
        fixture
            .bind_user("alice", &fixture.left)
            .try_acquire()
            .await
            .unwrap()
            .is_none()
    );
    drop(next);
    assert!(
        fixture
            .bind_user("alice", &fixture.left)
            .try_acquire()
            .await
            .unwrap()
            .is_some()
    );
    std::fs::write(release_path, "release").unwrap();
    release(child).await;
}
