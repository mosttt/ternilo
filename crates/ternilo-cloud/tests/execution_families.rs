use ternilo_cloud::{CloudSessionRecord, CloudStore};
use ternilo_control::{ControlStore, SecretCipher};
use ternilo_protocol::{ErrorCode, SessionId};

#[path = "support/execution_family_fixture.rs"]
mod fixture;
#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;

use fixture::{Fixture, request};

#[tokio::test]
async fn sqlite_execution_families_survive_turns_and_history_without_sharing_authority() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("families.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([53; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    Box::pin(contract(control, cloud)).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_execution_families_enforce_the_same_contract_with_runtime_rls() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_cloud_test"));
    let url =
        server_runtime::initialize(&admin_url, "ternilo_families_runtime_test", [53; 32]).await;
    let control =
        ControlStore::connect(&url, Some(&admin_url), SecretCipher::from_key([53; 32]), 4)
            .await
            .unwrap();
    let cloud = CloudStore::connect(&url, Some(&admin_url), 4)
        .await
        .unwrap();
    Box::pin(contract(control, cloud)).await;
    server_runtime::assert_scoped_without_schema_access(&url).await;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_execution_families")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        visible, 0,
        "an unscoped runtime connection cannot inspect directory families"
    );
    assert!(
        sqlx::query("UPDATE cloud_execution_families SET family_id='forged'")
            .execute(&pool)
            .await
            .is_err(),
        "runtime grants cannot rewrite persistent family identities"
    );
    assert!(
        sqlx::query("DELETE FROM cloud_execution_families")
            .execute(&pool)
            .await
            .is_err(),
        "removing visible history must not allow runtime deletion of family identities"
    );
    pool.close().await;
}

async fn contract(control: ControlStore, cloud: CloudStore) {
    let mut fixture = Fixture::open(control, cloud).await;
    let parent = fixture.session("family-parent").await;
    let independent = fixture.session("family-independent").await;
    let original = fixture.family(&parent.session_id).await;
    if fixture.cloud.database().backend() == ternilo_storage::Backend::Postgres {
        let mut tx = fixture
            .cloud
            .database()
            .tenant_transaction(&fixture.tenant)
            .await
            .unwrap();
        ternilo_storage::set_user_scope(&mut tx, &fixture.actor.user_id)
            .await
            .unwrap();
        let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_execution_families")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            visible, 0,
            "tenant membership does not expose another owner's family metadata"
        );
        tx.commit().await.unwrap();
    }
    assert_eq!(original.0, parent.session_id.as_str());
    assert_eq!(original.1, fixture.owner.user_id.as_str());
    assert_eq!(original.2, parent.workspace_id.as_str());
    assert_ne!(
        fixture.family(&independent.session_id).await.0,
        original.0,
        "sessions in the same workspace start separate execution families"
    );
    fixture.share(&parent.session_id).await;
    let actor = fixture.actor.user_id.clone();
    let run_id = fixture.submit(&parent, "family-parent-turn", &actor).await;
    let parent_run = fixture.start(&run_id).await;
    let (child, child_id) = fixture.child(&parent_run, "family-child").await;
    let child_mapping = fixture.family(&child.session_id).await;
    assert_eq!(child_mapping.0, original.0);
    let child_run = fixture.start(&child_id).await;
    let (grandchild, grandchild_id) = fixture.child(&child_run, "family-grandchild").await;
    assert_eq!(fixture.family(&grandchild.session_id).await.0, original.0);
    let grandchild_run = fixture.start(&grandchild_id).await;
    for run in [&parent_run, &child_run, &grandchild_run] {
        assert_eq!(run.claim.actor_user_id, actor);
        assert_eq!(run.claim.spec.metadata.user_id, fixture.owner.user_id);
        assert_eq!(run.claim.authorization_session_id, parent.session_id);
    }
    fixture.finish(&grandchild_run).await;
    fixture.finish(&child_run).await;
    fixture.finish(&parent_run).await;
    ordinary_forks_are_independent(&mut fixture, &parent, &child).await;
    manual_turns_preserve_family_without_inheriting_authority(&mut fixture, &parent, &child).await;
    assert_eq!(fixture.family(&parent.session_id).await, original);
    assert_eq!(
        fixture.family(&child.session_id).await,
        child_mapping,
        "ordinary child inbox submissions must not overwrite their original family registration"
    );
    implicit_inbox_creation_registers_a_new_family(&mut fixture, &parent).await;
    history_deletion_cannot_recycle_family_identity(&mut fixture, &grandchild).await;
    history_deletion_cannot_recycle_family_identity(&mut fixture, &independent).await;
}

async fn ordinary_forks_are_independent(
    fixture: &mut Fixture,
    parent: &CloudSessionRecord,
    child: &CloudSessionRecord,
) {
    let original = fixture.family(&parent.session_id).await.0;
    let mut families = std::collections::BTreeSet::from([original]);
    for source in [parent, child] {
        let now = fixture.tick();
        let fork = fixture
            .cloud
            .fork_session(
                &fixture.tenant,
                &fixture.owner.user_id,
                &source.session_id,
                None,
                now,
            )
            .await
            .unwrap();
        assert_eq!(fork.parent_session_id.as_ref(), Some(&source.session_id));
        assert!(fork.subagent.is_none());
        let family = fixture.family(&fork.session_id).await.0;
        assert_eq!(family, fork.session_id.as_str());
        assert!(
            families.insert(family),
            "a user fork must not join its source's cooperation family"
        );
    }
}

async fn manual_turns_preserve_family_without_inheriting_authority(
    fixture: &mut Fixture,
    parent: &CloudSessionRecord,
    child: &CloudSessionRecord,
) {
    let actor = fixture.actor.user_id.clone();
    let owner = fixture.owner.user_id.clone();
    let compiled = fixture.compiled(child, "unshared-child-turn", &actor);
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .enqueue_session_submission_as(&actor, &compiled, &request(&compiled), now)
            .await
            .is_err(),
        "sharing only the parent does not grant human access to its child session"
    );

    let mut forged = fixture.compiled(parent, "forged-owner-turn", &actor);
    forged.spec.metadata.user_id = actor.clone();
    let now = fixture.tick();
    assert_eq!(
        fixture
            .cloud
            .enqueue_session_submission_as(&actor, &forged, &request(&forged), now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied,
        "sharing a session does not allow changing its resource owner"
    );

    let parent_turn = fixture.submit(parent, "parent-next-turn", &actor).await;
    let run = fixture.start(&parent_turn).await;
    fixture.finish(&run).await;
    let own_turn = fixture.submit(child, "owner-child-followup", &owner).await;
    let run = fixture.start(&own_turn).await;
    assert_eq!(run.claim.authorization_session_id, child.session_id);
    assert_eq!(run.claim.actor_user_id, owner);
    fixture.finish(&run).await;

    fixture.share(&child.session_id).await;
    let shared_turn = fixture.submit(child, "shared-child-followup", &actor).await;
    let run = fixture.start(&shared_turn).await;
    assert_eq!(
        run.claim.authorization_session_id, child.session_id,
        "a human follow-up uses its own current child grant instead of the original parent's authority"
    );
    assert_eq!(run.claim.actor_user_id, actor);
    assert_eq!(run.claim.spec.metadata.user_id, owner);
    let lineage = fixture
        .cloud
        .run_lineage(&fixture.tenant, &owner, &shared_turn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lineage.root_run_id, shared_turn);
    assert!(
        lineage.parent_run_id.is_none(),
        "persistent directory cooperation is separate from automatic run ancestry"
    );
    fixture.finish(&run).await;
}

async fn implicit_inbox_creation_registers_a_new_family(
    fixture: &mut Fixture,
    template: &CloudSessionRecord,
) {
    let session = SessionId::new("implicit-inbox-session");
    let mut compiled = fixture.compiled(template, "implicit-inbox-turn", &fixture.owner.user_id);
    compiled.spec.metadata.session_id = session.clone();
    compiled.authorization_session_id = session.clone();
    let reservation = fixture.reserve(&compiled).await;
    let now = fixture.tick();
    let receipt = fixture
        .cloud
        .enqueue_session_submission(&compiled, &reservation, &request(&compiled), now)
        .await
        .unwrap();
    let original = fixture.family(&session).await;
    assert_eq!(original.0, session.as_str());
    assert_ne!(original.0, fixture.family(&template.session_id).await.0);
    let run = fixture.start(&receipt.run.run_id).await;
    fixture.finish(&run).await;
    let record = fixture
        .cloud
        .get_session(&fixture.tenant, &session)
        .await
        .unwrap();
    let owner = fixture.owner.user_id.clone();
    let next = fixture.submit(&record, "implicit-next-turn", &owner).await;
    let run = fixture.start(&next).await;
    fixture.finish(&run).await;
    assert_eq!(
        fixture.family(&session).await,
        original,
        "ordinary inbox reuse must keep the first registration"
    );
}

async fn history_deletion_cannot_recycle_family_identity(
    fixture: &mut Fixture,
    session: &CloudSessionRecord,
) {
    let original = fixture.family(&session.session_id).await;
    let now = fixture.tick();
    fixture
        .cloud
        .delete_session(
            &fixture.tenant,
            &session.session_id,
            &fixture.owner.user_id,
            now,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .get_session(&fixture.tenant, &session.session_id)
            .await
            .is_err()
    );
    assert_eq!(
        fixture.family(&session.session_id).await,
        original,
        "deleting visible session history retains its immutable family identity"
    );
    let now = fixture.tick();
    assert_eq!(
        fixture
            .cloud
            .create_session(
                fixture.draft(&session.session_id),
                &fixture.tenant,
                &fixture.owner.user_id,
                now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
        "a new session cannot inherit a deleted session's persistent identity"
    );
    assert!(
        fixture
            .cloud
            .get_session(&fixture.tenant, &session.session_id)
            .await
            .is_err(),
        "a rejected reuse must roll back the new session row"
    );

    let compiled = fixture.compiled(
        session,
        &format!("reused-{}", session.session_id),
        &fixture.owner.user_id,
    );
    let reservation = fixture.reserve(&compiled).await;
    let now = fixture.tick();
    assert_eq!(
        fixture
            .cloud
            .enqueue_session_submission(&compiled, &reservation, &request(&compiled), now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
        "implicit inbox creation cannot bypass the deleted session identity boundary"
    );
    assert!(
        fixture
            .cloud
            .get_session(&fixture.tenant, &session.session_id)
            .await
            .is_err()
    );
    assert_eq!(fixture.family(&session.session_id).await, original);
}
