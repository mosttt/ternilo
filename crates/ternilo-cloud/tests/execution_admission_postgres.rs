use sqlx::Row;
use std::{collections::BTreeSet, time::Duration};
use ternilo_cloud::{
    CloudRunState, CloudSessionDraft, CloudSessionRecord, CloudStore, CloudWorkerIdentity,
    CompiledRun, ExecutionPhase, RunAdmission, RunLease, StartedRun, TerminalState, WorkerCapacity,
    WorkerRegisterRequest,
};
use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, OidcPrincipal, ResourceKind,
    ResourcePermissions, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::{
    AcceptedSubagentRun, AgentId, PermissionPreset, Profile, RunId, RunLimits, RunMetadata,
    RunOutcome, RunSpec, SessionEvent, SessionEventKind, SessionId, SessionMode,
    SessionSubmissionRequest, SubagentId, SubagentSessionMetadata, SubagentTranscriptKind,
    SubmissionContent, SubmissionDelivery, TenantId,
};
use ternilo_transport::{ExecutorCapability, ExecutorHello, ExecutorId, ExecutorKind};

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;

const NOW: u64 = 2_200_000_000_000;
const LEASE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn sqlite_execution_admission_separates_budgets_and_bounded_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("capacity.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([51; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    Box::pin(contract(control, cloud)).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_execution_admission_enforces_the_same_contract_with_runtime_rls() {
    let owner_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(owner_url.contains("ternilo_cloud_test"));
    let url =
        server_runtime::initialize(&owner_url, "ternilo_capacity_runtime_test", [51; 32]).await;
    let control =
        ControlStore::connect(&url, Some(&owner_url), SecretCipher::from_key([51; 32]), 4)
            .await
            .unwrap();
    let cloud = CloudStore::connect(&url, Some(&owner_url), 4)
        .await
        .unwrap();
    Box::pin(contract(control, cloud)).await;
    server_runtime::assert_scoped_without_schema_access(&url).await;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    for table in [
        "cloud_run_execution",
        "cloud_run_wait_dependencies",
        "cloud_workspace_epochs",
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0, "unscoped runtime access cannot reveal {table}");
    }
    assert!(
        sqlx::query("DELETE FROM cloud_workspace_epochs")
            .execute(&pool)
            .await
            .is_err(),
        "runtime credentials cannot reset physical workspace epochs"
    );
    pool.close().await;
}

#[path = "support/execution_admission/activity.rs"]
mod activity;
#[path = "support/execution_admission/capacity.rs"]
mod capacity;
#[path = "support/execution_admission/claims.rs"]
mod claims;
#[path = "support/execution_admission/epochs.rs"]
mod epochs;
#[path = "support/execution_admission/fixture.rs"]
mod fixture;
#[path = "support/execution_admission/pressure.rs"]
mod pressure;
#[path = "support/execution_admission/recovery.rs"]
mod recovery;

#[path = "support/execution_admission/workspace_recovery.rs"]
mod workspace_recovery;
#[path = "support/execution_admission/workspace_waiting.rs"]
mod workspace_waiting;

use activity::*;
use capacity::*;
use fixture::{Fixture, registration};
use pressure::*;
use recovery::*;

async fn contract(control: ControlStore, cloud: CloudStore) {
    let mut fixture = Fixture::open(control, cloud).await;
    epochs::workspace_epochs_are_durable_and_tickets_are_bound_to_the_run(&mut fixture).await;
    claims::unstarted_claims_release_ownership_but_started_runs_require_confirmation(&mut fixture)
        .await;
    workspace_recovery::recovery_preserves_old_identity_and_cannot_release_a_new_epoch(
        &mut fixture,
    )
    .await;
    workspace_recovery::another_worker_recovers_an_expired_resident_without_replaying_it(
        &mut fixture,
    )
    .await;
    workspace_recovery::recovery_cursor_advances_past_unconfirmed_first_page(&mut fixture).await;
    workspace_waiting::blocked_directories_do_not_hide_ready_workspaces(&mut fixture).await;
    workspace_waiting::expired_members_block_their_own_family_until_physical_confirmation(
        &mut fixture,
    )
    .await;
    workspace_occupancy_blocks_independent_families(&mut fixture).await;
    four_parents_and_nested_children(&mut fixture).await;
    foreground_claim_race(&mut fixture).await;
    execution_activity_projection(&mut fixture).await;
    queued_backlog_cannot_hide_other_sessions(&mut fixture).await;
    deep_resident_pressure(&mut fixture, OtherWorker::Idle).await;
    deep_resident_pressure(&mut fixture, OtherWorker::Active).await;
    deep_resident_pressure(&mut fixture, OtherWorker::Absent).await;
    postgres_reaper_skips_sessions_waiting_for_the_quota(&mut fixture).await;
    expired_residents_and_generation_fences(&mut fixture).await;
}
