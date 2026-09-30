use std::{collections::BTreeSet, time::Duration};

use ternilo_cloud::{CloudRunDraft, CloudStore, CloudSubmissionReceipt, WorkerPolicy};
use ternilo_control::{
    ControlStore, ControlUser, OidcPrincipal, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_kernel::Catalog;
use ternilo_protocol::{
    AgentId, AutomatedInputSource, InputAuthor, PermissionPreset, Profile, RunId, RunLimits,
    SessionId, SessionMode, SessionSubmissionRequest, SubmissionContent, SubmissionDelivery,
    SubmissionPlacement, TenantId, WorkspaceId,
};

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;

const NOW: u64 = 2_000_000_000_000;

#[tokio::test]
async fn sqlite_batches_preserve_author_boundaries_and_fifo() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("authors.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([23; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    batch_contract(control, cloud).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_batches_preserve_author_boundaries_and_fifo() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set");
    assert!(admin_url.contains("ternilo_cloud_test"));
    let runtime_url =
        server_runtime::initialize(&admin_url, "ternilo_batch_authors_test", [23; 32]).await;
    let control = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([23; 32]),
        4,
    )
    .await
    .unwrap();
    let cloud = CloudStore::connect(&runtime_url, Some(&admin_url), 4)
        .await
        .unwrap();
    batch_contract(control, cloud).await;
    server_runtime::assert_scoped_without_schema_access(&runtime_url).await;
}

#[derive(Clone, Copy)]
enum Author {
    Owner,
    RenamedOwner,
    Member,
    Local,
    Schedule,
    Subagent,
    Unknown,
    DifferentLimits,
    DifferentMonth,
}

struct Fixture {
    control: ControlStore,
    cloud: CloudStore,
    owner: ControlUser,
    member: ControlUser,
    tenant: TenantId,
    project: String,
    workspace: WorkspaceId,
}

async fn batch_contract(control: ControlStore, cloud: CloudStore) {
    use Author::{
        DifferentLimits, DifferentMonth, Local, Member, Owner, RenamedOwner, Schedule, Subagent,
        Unknown,
    };

    let owner = user(&control, "batch-owner").await;
    let member = user(&control, "batch-member").await;
    let tenant = control
        .create_tenant(
            &owner,
            "batch-authors",
            "Batch authors",
            TenantQuota {
                max_nodes: 2,
                max_concurrent_runs: 128,
                monthly_model_tokens: 100_000,
                max_secrets: 4,
            },
            NOW + 1,
        )
        .await
        .unwrap()
        .tenant_id;
    control
        .set_membership(
            &owner,
            &tenant,
            &member.user_id,
            TenantRole::Member,
            NOW + 2,
        )
        .await
        .unwrap();
    let project = control
        .create_project(&owner, &tenant, "Batch authors", NOW + 3)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(
            &owner,
            &tenant,
            &project.project_id,
            "Batch authors",
            NOW + 4,
        )
        .await
        .unwrap();
    let fixture = Fixture {
        control,
        cloud,
        owner,
        member,
        tenant,
        project: project.project_id,
        workspace: workspace.workspace_id,
    };
    let cases: &[(&[Author], usize)] = &[
        (&[Owner, Owner, Owner], 3),
        (&[Owner, RenamedOwner, Member, Owner], 2),
        (&[Owner, Member, Owner], 1),
        (&[Local, Local, Owner, Local], 2),
        (&[Schedule, Schedule, Owner], 1),
        (&[Subagent, Subagent, Owner], 1),
        (&[Unknown, Unknown, Owner], 1),
        (&[Owner, Unknown, Owner], 1),
        (&[Owner, Schedule, Owner], 1),
        (&[Owner, DifferentLimits, Owner], 1),
        (&[Owner, DifferentMonth, Owner], 1),
    ];
    for (case, &(authors, prefix)) in cases.iter().enumerate() {
        fixture.assert_batch(case, authors, prefix).await;
    }
}

impl Fixture {
    async fn assert_batch(&self, case: usize, authors: &[Author], prefix: usize) {
        let session = SessionId::new(format!("batch-authors-{case}"));
        let blocker = self.enqueue(&session, "blocker", Author::Owner).await;
        self.cloud
            .pause_session_inbox(&self.tenant, &self.owner.user_id, &session, None, NOW + 10)
            .await
            .unwrap();
        let mut runs = Vec::new();
        for (index, author) in authors.iter().enumerate() {
            runs.push(self.enqueue(&session, &index.to_string(), *author).await);
        }
        self.cloud
            .cancel_run_as(
                &self.tenant,
                &self.owner.user_id,
                &session,
                &blocker,
                NOW + 20,
            )
            .await
            .unwrap();
        self.cloud
            .resume_session_inbox(&self.tenant, &self.owner.user_id, &session, NOW + 21)
            .await
            .unwrap();
        let reopened = CloudStore::from_database(self.cloud.database().clone())
            .await
            .unwrap();
        let inbox = reopened
            .session_inbox(&self.tenant, &self.owner.user_id, &session)
            .await
            .unwrap();
        assert_eq!(inbox.active_run_id.as_ref(), Some(&runs[0]));
        assert_eq!(inbox.items.len(), authors.len());
        for (index, (item, run)) in inbox.items.iter().zip(&runs).enumerate() {
            assert_eq!(
                &item.run_id, run,
                "FIFO must preserve every accepted occurrence"
            );
            assert_eq!(
                item.placement,
                if index < prefix {
                    SubmissionPlacement::Running
                } else {
                    SubmissionPlacement::Queued
                },
                "case {case}, input {index}: a batch must stop at the first author or policy boundary"
            );
        }
    }

    async fn enqueue(&self, session: &SessionId, suffix: &str, author: Author) -> RunId {
        let run_id = RunId::new(format!("{session}-{suffix}"));
        let actor = if matches!(author, Author::Member) {
            &self.member
        } else {
            &self.owner
        };
        let mut compiled = policy()
            .compile_run(
                CloudRunDraft {
                    project_id: self.project.clone(),
                    workspace_id: self.workspace.clone(),
                    agent_id: AgentId::new("agent"),
                    session_id: session.clone(),
                    run_id: Some(run_id.clone()),
                    limits: RunLimits {
                        max_steps: 2,
                        max_tool_calls: 4,
                    },
                    permissions: PermissionPreset::WorkspaceWrite,
                    mode: SessionMode::Execute,
                    profile: Profile::default(),
                    input: suffix.to_owned(),
                    references: Vec::new(),
                    reference_contexts: Vec::new(),
                    attachments: Vec::new(),
                    reserved_model_tokens: 100,
                },
                self.tenant.clone(),
                self.owner.user_id.clone(),
                actor.user_id.clone(),
                &Catalog::new("batch-author-catalog"),
            )
            .unwrap();
        if matches!(author, Author::DifferentLimits) {
            compiled.spec.limits.max_steps = 3;
        }
        compiled.automated_input = match author {
            Author::Schedule => Some(AutomatedInputSource::Schedule),
            Author::Subagent => Some(AutomatedInputSource::Subagent),
            _ => None,
        };
        let reservation = self
            .control
            .reserve_quota(
                &self.owner,
                &self.tenant,
                Some(run_id.as_str()),
                100,
                Duration::from_hours(1),
                NOW + 5,
            )
            .await
            .unwrap();
        let receipt = self
            .cloud
            .enqueue_session_submission(
                &compiled,
                &reservation.reservation_id,
                &SessionSubmissionRequest {
                    delivery: SubmissionDelivery::Queue,
                    run_id: Some(run_id.clone()),
                    content: SubmissionContent::Prompt {
                        input: suffix.to_owned(),
                    },
                    references: Vec::new(),
                    attachments: Vec::new(),
                },
                NOW + 6,
            )
            .await
            .unwrap();
        self.adapt_persisted_input(author, receipt, &reservation.reservation_id)
            .await;
        run_id
    }

    async fn adapt_persisted_input(
        &self,
        author: Author,
        receipt: CloudSubmissionReceipt,
        reservation_id: &str,
    ) {
        if matches!(
            author,
            Author::Local | Author::Unknown | Author::RenamedOwner | Author::DifferentMonth
        ) {
            let mut transaction = self
                .cloud
                .database()
                .owner_transaction(&self.tenant, &self.owner.user_id)
                .await
                .unwrap();
            if matches!(author, Author::DifferentMonth) {
                sqlx::query("UPDATE control_quota_reservations SET period_start='1900-01' WHERE tenant_id=$1 AND reservation_id=$2")
                    .bind(self.tenant.as_str()).bind(reservation_id).execute(&mut *transaction).await.unwrap();
            } else {
                // Represent trusted historical/local inputs that the browser enqueue API does not create.
                let mut provenance = receipt.submission.provenance;
                if matches!(author, Author::Unknown) {
                    provenance = None;
                } else {
                    provenance.as_mut().unwrap().author = if matches!(author, Author::Local) {
                        InputAuthor::Local
                    } else {
                        InputAuthor::Account {
                            user_id: self.owner.user_id.clone(),
                            username: "renamed-owner".to_owned(),
                        }
                    };
                }
                for statement in [
                    "UPDATE cloud_session_submissions SET input_provenance=$3 WHERE tenant_id=$1 AND run_id=$2",
                    "UPDATE cloud_runs SET input_provenance=$3 WHERE tenant_id=$1 AND run_id=$2",
                ] {
                    sqlx::query(statement)
                        .bind(self.tenant.as_str())
                        .bind(receipt.submission.run_id.as_str())
                        .bind(provenance.as_ref().map(ternilo_storage::Json))
                        .execute(&mut *transaction)
                        .await
                        .unwrap();
                }
            }
            transaction.commit().await.unwrap();
        }
    }
}

async fn user(control: &ControlStore, name: &str) -> ControlUser {
    control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://batch-author.example".to_owned(),
                subject: name.to_owned(),
                email: None,
                display_name: Some(name.to_owned()),
            },
            name,
            NOW,
        )
        .await
        .unwrap()
}

fn policy() -> WorkerPolicy {
    WorkerPolicy {
        catalog_revision: "batch-author-catalog".to_owned(),
        policy_revision: "batch-author-policy".to_owned(),
        maximum_limits: RunLimits {
            max_steps: 4,
            max_tool_calls: 8,
        },
        max_run_attempts: 2,
        max_tenant_workspace_bytes: 1024 * 1024 * 1024,
        max_tenant_workspace_entries: 100_000,
        minimum_workspace_free_bytes: 0,
        allowed_plugin_kinds: BTreeSet::default(),
        max_extension_packages_per_run: 0,
        extension_host_policy: ternilo_extension::ExtensionHostPolicy::default(),
        denied_tools: BTreeSet::default(),
    }
}
