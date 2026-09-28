use std::time::Duration;

use ternilo_cloud::{CloudLiveNotification, CloudSessionEventFeed, CloudStore};
use ternilo_control::{ControlStore, SecretCipher};
use ternilo_storage::Database;
use tokio::sync::broadcast;

async fn database() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("live.sqlite3").display()
    );
    let database = Database::connect(&url, 3).await.unwrap();
    ControlStore::from_database(database.clone(), SecretCipher::from_key([31; 32]))
        .await
        .unwrap();
    CloudStore::from_database(database.clone()).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO control_users (user_id, issuer, subject, email, username, created_at_ms, last_seen_at_ms)
         VALUES ('user', 'native', 'user', NULL, 'user', 0, 0);
         INSERT INTO control_tenants (tenant_id, slug, display_name, kind, created_by, created_at_ms)
         VALUES ('tenant', 'tenant', 'Tenant', 'team', 'user', 0);
         INSERT INTO control_projects VALUES ('tenant', 'project', 'Project', 'user', 0);
         INSERT INTO control_workspaces
             (tenant_id, workspace_id, project_id, owner_user_id, name, placement,
              storage, created_at_ms, updated_at_ms)
         VALUES ('tenant', 'workspace', 'project', 'user', 'Workspace', 'cloud',
                 'cloud_volume', 0, 0);
         INSERT INTO cloud_sessions
             (tenant_id, session_id, user_id, project_id, workspace_id, agent_id,
              state, created_at_ms, updated_at_ms)
         VALUES ('tenant', 'session', 'user', 'project', 'workspace', 'agent', 'idle', 0, 0);",
    )
    .execute(database.pool())
    .await
    .unwrap();
    (directory, database)
}

async fn receive(
    receiver: &mut broadcast::Receiver<CloudLiveNotification>,
) -> CloudLiveNotification {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("committed change must wake the feed")
        .unwrap()
}

async fn assert_quiet(receiver: &mut broadcast::Receiver<CloudLiveNotification>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(220), receiver.recv())
            .await
            .is_err(),
        "idle polling and uncommitted changes must not emit invalidations",
    );
}

#[tokio::test]
async fn sqlite_feed_is_transactional_precise_shared_and_closeable() {
    let (_directory, database) = database().await;
    let feed = CloudSessionEventFeed::from_database(database.clone())
        .await
        .unwrap();
    let retained = feed.clone();
    let mut first = feed.subscribe();
    let mut second = retained.subscribe();
    drop(feed);
    assert_quiet(&mut first).await;

    let mut transaction = database.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO cloud_session_events VALUES
         ('tenant', 'session', 0, 'run', $1, 1, 1)",
    )
    .bind(r#"{"type":"assistant_message_delta","delta":"private event body"}"#)
    .execute(&mut *transaction)
    .await
    .unwrap();
    assert_quiet(&mut first).await;
    transaction.commit().await.unwrap();
    for receiver in [&mut first, &mut second] {
        let change = receive(receiver).await;
        assert!(matches!(
            change,
            CloudLiveNotification::Session {
                tenant_id, user_id: Some(user_id), session_id: Some(session_id),
                dirty, activity: false, workbench: false,
            } if tenant_id.as_str() == "tenant" && user_id.as_str() == "user"
                && session_id.as_str() == "session" && dirty.events && dirty.metadata().is_empty()
        ));
    }

    sqlx::query("UPDATE cloud_sessions SET last_seq = 1, updated_at_ms = 1")
        .execute(database.pool())
        .await
        .unwrap();
    assert_quiet(&mut first).await;
    sqlx::query("UPDATE cloud_sessions SET state = 'running'")
        .execute(database.pool())
        .await
        .unwrap();
    assert!(matches!(receive(&mut first).await,
        CloudLiveNotification::Session { dirty, activity: true, workbench: false, .. }
            if dirty.is_empty()));

    let mut transaction = database.begin().await.unwrap();
    sqlx::query("UPDATE cloud_sessions SET title = 'Uncommitted title'")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    assert_quiet(&mut first).await;

    drop(retained);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), first.recv())
            .await
            .unwrap(),
        Err(broadcast::error::RecvError::Closed)
    ));
    database.close().await;
}

#[tokio::test]
async fn sqlite_feed_preserves_agent_team_owner_scope_and_drains_streaming_batches() {
    let (_directory, database) = database().await;
    let feed = CloudSessionEventFeed::from_database(database.clone())
        .await
        .unwrap();
    let mut changes = feed.subscribe();
    sqlx::raw_sql(
        "INSERT INTO cloud_agent_team_tasks
             (tenant_id, user_id, team_id, task_id, subject, description, status,
              revision, created_at_ms, updated_at_ms)
         VALUES ('tenant', 'user', 'team', 'first', 'First', '', 'pending', 1, 0, 0),
                ('tenant', 'user', 'team', 'second', 'Second', '', 'pending', 1, 0, 0);
         INSERT INTO cloud_agent_team_task_dependencies VALUES
             ('tenant', 'user', 'team', 'first', 'second');
         INSERT INTO cloud_agent_team_messages
             (tenant_id, user_id, team_id, message_id, from_member_id, to_member_id,
              content, created_at_ms)
         VALUES ('tenant', 'user', 'team', 'message', 'first', 'second', 'Private message', 0);
         UPDATE cloud_agent_team_tasks SET subject = 'Updated' WHERE task_id = 'first';
         DELETE FROM cloud_agent_team_messages;
         DELETE FROM cloud_agent_team_task_dependencies;",
    )
    .execute(database.pool())
    .await
    .unwrap();
    for _ in 0..7 {
        assert!(matches!(receive(&mut changes).await,
            CloudLiveNotification::Session {
                tenant_id, user_id: Some(user_id), session_id: None, dirty,
                activity: false, workbench: false,
            } if tenant_id.as_str() == "tenant" && user_id.as_str() == "user"
                && dirty.agent_team && !dirty.events && !dirty.inbox));
    }

    // A batch larger than the scan limit must drain without waiting one interval per batch.
    sqlx::query(
        "WITH RECURSIVE numbers(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM numbers WHERE n < 599)
         INSERT INTO cloud_session_events
             SELECT 'tenant', 'session', n, 'run',
                    '{\"type\":\"assistant_message_delta\",\"delta\":\"private\"}', 1, 1
             FROM numbers",
    )
    .execute(database.pool())
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        for _ in 0..600 {
            assert!(matches!(receive(&mut changes).await,
                CloudLiveNotification::Session { dirty, workbench: false, activity: false, .. }
                    if dirty.events && dirty.metadata().is_empty()));
        }
    })
    .await
    .expect("every committed event remains observable across scan batches");
    assert_quiet(&mut changes).await;
    drop(feed);
    database.close().await;
}
