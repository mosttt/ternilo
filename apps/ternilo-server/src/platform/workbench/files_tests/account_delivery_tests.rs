use super::*;
use crate::gateway_journal::{GatewayJournal, RouteKey};
use ternilo_control::AccountStatusAction;
use ternilo_protocol::{InputAuthor, InputProvenance, SubmissionId};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandOutcome, CommandReply, ExecutorCommand,
    ExecutorCommandBody, ExecutorScope,
};

fn input(
    fixture: &Fixture,
    session: &EdgeSessionRecord,
    actor: &ternilo_control::ControlUser,
    id: &str,
) -> ExecutorCommand {
    let run_id = RunId::new(format!("run-{id}"));
    ExecutorCommand {
        command_id: CommandId::new(id),
        scope: ExecutorScope {
            tenant_id: fixture.tenant.clone(),
            user_id: fixture.owner.session.user.user_id.clone(),
        },
        input_provenance: Some(InputProvenance {
            input_id: SubmissionId::new(format!("input-{id}")),
            run_id: Some(run_id.clone()),
            author: InputAuthor::Account {
                user_id: actor.user_id.clone(),
                username: actor.username.clone(),
            },
        }),
        issued_at_ms: fixture.now,
        expires_at_ms: fixture.now + 10_000,
        body: ExecutorCommandBody::Application {
            request: ApplicationOperation::SessionTurn {
                session_id: session.node_session_id.clone(),
                input: "/agents".to_owned(),
                run_id: Some(run_id.to_string()),
                attachments: vec![],
            },
        },
    }
}

async fn register_node(fixture: &Fixture, session: &EdgeSessionRecord) {
    use ternilo_transport::{
        EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorHello, ExecutorKind,
    };
    fixture
        .state
        .store
        .edge_store()
        .register_executor(
            &fixture.tenant,
            &ExecutorHello {
                protocol_version: EXECUTOR_PROTOCOL_VERSION,
                executor_id: session.executor_id.clone(),
                executor_kind: ExecutorKind::EdgeNode,
                instance_nonce: "delivery-contract".to_owned(),
                catalog_revision: "delivery-contract".to_owned(),
                capabilities: [
                    ExecutorCapability::ApplicationRpc,
                    ExecutorCapability::SessionEventDelta,
                    ExecutorCapability::LiveInvalidations,
                ]
                .into_iter()
                .collect(),
            },
            fixture.now,
        )
        .await
        .unwrap();
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep durable ban/unban, pending rejection, inflight evidence and cross-account delivery in one database contract."
)]
async fn contract(fixture: &Fixture) {
    let session = fixture.edge_session("account-delivery").await;
    register_node(fixture, &session).await;
    let collaborator = fixture.collaborator().await;
    let member = &collaborator.session.user;
    let owner = &fixture.owner.session.user;
    let store = &fixture.state.store;
    let route = RouteKey::new(fixture.tenant.clone(), session.executor_id.clone());
    let journal = GatewayJournal::open(store.database().clone())
        .await
        .unwrap();
    let lease = journal
        .acquire(&route, "delivery-test", fixture.now, 20_000)
        .await
        .unwrap()
        .unwrap();
    let inflight = input(fixture, &session, member, "00-inflight");
    journal.enqueue(&route, &inflight).await.unwrap();
    assert_eq!(
        journal
            .claim(&route, &lease, fixture.now, 10, 1)
            .await
            .unwrap(),
        vec![inflight.clone()]
    );
    let pending = input(fixture, &session, member, "01-pending");
    let after_unban = input(fixture, &session, member, "02-before-unban");
    let healthy = input(fixture, &session, owner, "99-owner");
    for command in [&pending, &after_unban, &healthy] {
        journal.enqueue(&route, command).await.unwrap();
    }
    let banned = store
        .set_account_status(
            owner,
            &member.user_id,
            AccountStatusAction::Ban,
            1,
            fixture.now + 1,
        )
        .await
        .unwrap();
    let denied = input(fixture, &session, member, "denied-during-ban");
    assert!(journal.enqueue(&route, &denied).await.is_err());
    assert!(
        !journal.contains(&route, &denied.command_id).await.unwrap(),
        "failed admission is atomic"
    );
    // Re-enable before any dispatch: comparing only current active status would replay old work.
    store
        .set_account_status(
            owner,
            &member.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            fixture.now + 2,
        )
        .await
        .unwrap();
    journal.enqueue(&route, &after_unban).await.unwrap();
    assert_eq!(
        journal
            .claim(&route, &lease, fixture.now + 11, 10, 1)
            .await
            .unwrap(),
        vec![healthy.clone()],
        "revoked rows must not starve another author's input in a one-item batch"
    );
    for command in [&pending, &after_unban] {
        let reply = journal
            .reply(&route, &command.command_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(reply.outcome, CommandOutcome::Error { error } if error.code == ternilo_protocol::ErrorCode::PolicyDenied && error.message.contains("not dispatched"))
        );
    }
    assert!(
        journal
            .reply(&route, &inflight.command_id)
            .await
            .unwrap()
            .is_none(),
        "redelivery prevention must not invent a stopped outcome"
    );
    let actual = CommandReply::success(
        inflight.command_id.clone(),
        fixture.now + 12,
        json!({"actual":"already executed"}),
    );
    journal
        .complete(&route, &lease, &actual, fixture.now + 12)
        .await
        .unwrap();
    assert_eq!(
        journal.reply(&route, &inflight.command_id).await.unwrap(),
        Some(actual)
    );
    let healthy_reply =
        CommandReply::success(healthy.command_id.clone(), fixture.now + 12, json!(null));
    journal
        .complete(&route, &lease, &healthy_reply, fixture.now + 12)
        .await
        .unwrap();
    drop(journal);
    let reopened = GatewayJournal::open(store.database().clone())
        .await
        .unwrap();
    assert!(
        reopened
            .claim(&route, &lease, fixture.now + 30, 10, 1)
            .await
            .unwrap()
            .is_empty()
    );
    let fresh = input(fixture, &session, member, "fresh-after-unban");
    reopened.enqueue(&route, &fresh).await.unwrap();
    assert_eq!(
        reopened
            .claim(&route, &lease, fixture.now + 31, 10, 1)
            .await
            .unwrap(),
        vec![fresh.clone()]
    );
    // An older database may contain accepted commands without the new admission evidence.
    let legacy = input(fixture, &session, owner, "legacy-pending");
    reopened.enqueue(&route, &legacy).await.unwrap();
    let mut tx = store
        .database()
        .tenant_transaction(&fixture.tenant)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM gateway_input_authorizations WHERE tenant_id = $1 AND command_id = $2",
    )
    .bind(fixture.tenant.as_str())
    .bind(legacy.command_id.as_str())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        reopened
            .claim(&route, &lease, fixture.now + 32, 10, 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        reopened
            .reply(&route, &legacy.command_id)
            .await
            .unwrap()
            .is_some()
    );
    if store.database().backend() == ternilo_storage::Backend::Postgres {
        let mut tx = store
            .database()
            .tenant_transaction(&TenantId::new("different-tenant"))
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gateway_input_authorizations")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            count, 0,
            "runtime cannot read another tenant's authorization records"
        );
        tx.commit().await.unwrap();
    }
}

#[tokio::test]
async fn account_delivery_does_not_replay_revoked_inputs_or_invent_inflight_outcomes() {
    contract(&Fixture::new("sqlite::memory:", None).await).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_account_delivery_uses_restricted_runtime_authorization() {
    use sqlx::Executor as _;
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    sqlx::raw_sql("DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN CREATE ROLE ternilo_runtime NOLOGIN; END IF; END $$;
        DROP ROLE IF EXISTS account_delivery_runtime;
        CREATE ROLE account_delivery_runtime LOGIN PASSWORD 'account-delivery-password';
        GRANT ternilo_runtime TO account_delivery_runtime;
        REVOKE CREATE ON SCHEMA public FROM PUBLIC;
        GRANT USAGE ON SCHEMA public TO account_delivery_runtime;")
        .execute(&admin).await.unwrap();
    let mut runtime = reqwest::Url::parse(&url).unwrap();
    runtime.set_username("account_delivery_runtime").unwrap();
    runtime
        .set_password(Some("account-delivery-password"))
        .unwrap();
    let fixture = Fixture::new(runtime.as_str(), Some(&url)).await;
    contract(&fixture).await;
    fixture.state.store.database().close().await;
    admin.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep authenticated reconnect, withheld old inputs and successful new delivery in one transport lifecycle."
)]
async fn node_reconnect_dispatches_other_accounts_but_not_pre_ban_inputs() {
    use futures_util::SinkExt as _;
    use salvo_core::conn::tcp::TcpAcceptor;
    use ternilo_transport::{ControlFrame, ExecutorFrame};
    use tokio_tungstenite::tungstenite::Message;

    let fixture = Fixture::new("sqlite::memory:", None).await;
    let (session, credential) = fixture
        .edge_session_with_credential("revoked-delivery")
        .await;
    register_node(&fixture, &session).await;
    let collaborator = fixture.collaborator().await;
    let owner = &fixture.owner.session.user;
    let member = &collaborator.session.user;
    let route = RouteKey::new(fixture.tenant.clone(), session.executor_id.clone());
    let journal = GatewayJournal::open(fixture.state.store.database().clone())
        .await
        .unwrap();
    let old = input(&fixture, &session, member, "00-revoked");
    let healthy = input(&fixture, &session, owner, "99-independent");
    journal.enqueue(&route, &old).await.unwrap();
    journal.enqueue(&route, &healthy).await.unwrap();
    let banned = fixture
        .state
        .store
        .set_account_status(
            owner,
            &member.user_id,
            AccountStatusAction::Ban,
            1,
            fixture.now + 1,
        )
        .await
        .unwrap();
    fixture
        .state
        .store
        .set_account_status(
            owner,
            &member.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            fixture.now + 2,
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = crate::platform::web_router(fixture.state.clone());
    let serving = tokio::spawn(async move { server.try_serve(router).await.unwrap() });
    let (mut socket, _) = super::transport_tests::connect_upload_node(
        address,
        &session.executor_id,
        &credential,
        "1234567890abcdef1234567890abcdef",
    )
    .await;
    let ControlFrame::Command { command } = super::transport_tests::next_frame(&mut socket).await
    else {
        panic!("expected the independent owner's command")
    };
    assert_eq!(*command, healthy);
    assert!(matches!(
        journal
            .reply(&route, &old.command_id)
            .await
            .unwrap()
            .unwrap()
            .outcome,
        CommandOutcome::Error { .. }
    ));
    socket
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::Reply {
                reply: CommandReply::success(
                    command.command_id.clone(),
                    now_ms().unwrap(),
                    json!(null),
                ),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let ExecutorCommandBody::Application { request } =
        input(&fixture, &session, member, "fresh-wire").body
    else {
        unreachable!()
    };
    let fresh = fixture
        .state
        .edge
        .call_as(&fixture.tenant, &session.executor_id, member, request);
    let receive = async {
        let ControlFrame::Command { command } =
            super::transport_tests::next_frame(&mut socket).await
        else {
            panic!("expected new post-unban input")
        };
        assert_eq!(
            command.input_provenance.as_ref().unwrap().author,
            InputAuthor::Account {
                user_id: member.user_id.clone(),
                username: member.username.clone()
            }
        );
        socket
            .send(Message::Text(
                serde_json::to_string(&ExecutorFrame::Reply {
                    reply: CommandReply::success(
                        command.command_id,
                        now_ms().unwrap(),
                        json!({"accepted":"fresh"}),
                    ),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
    };
    let (result, ()) = tokio::join!(fresh, receive);
    assert_eq!(result.unwrap(), json!({"accepted":"fresh"}));
    socket.close(None).await.unwrap();
    handle.stop_graceful(Some(Duration::from_secs(1)));
    serving.await.unwrap();
}
