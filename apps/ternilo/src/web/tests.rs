use super::*;
use salvo_core::routing::PathState;

#[tokio::test]
async fn legacy_event_delta_http_route_is_not_mounted() {
    let mut request = Request::new();
    *request.uri_mut() = "http://local.test/api/v1/sessions/session/event-delta"
        .parse()
        .unwrap();
    let root = Router::with_path("api/v1").push(session_router());
    let mut path = PathState::from_owned_path(request.uri().path().to_owned());

    assert!(root.detect(&mut request, &mut path).await.is_none());
}

#[tokio::test]
async fn extension_revoke_and_uninstall_have_distinct_http_routes() {
    async fn detects(method: salvo_core::http::Method, path: &str) -> bool {
        let mut request = Request::new();
        *request.method_mut() = method;
        *request.uri_mut() = format!("http://local.test{path}").parse().unwrap();
        let root = Router::with_path("api/v1").push(extension_router());
        let mut path = PathState::from_owned_path(request.uri().path().to_owned());
        root.detect(&mut request, &mut path).await.is_some()
    }

    let package = "/api/v1/extensions/dev.example/1.0.0";
    assert!(detects(salvo_core::http::Method::DELETE, package).await);
    assert!(
        detects(
            salvo_core::http::Method::POST,
            "/api/v1/extensions/dev.example/1.0.0/revoke"
        )
        .await
    );
    assert!(!detects(salvo_core::http::Method::POST, package).await);
    assert!(
        !detects(
            salvo_core::http::Method::DELETE,
            "/api/v1/extensions/dev.example/1.0.0/revoke"
        )
        .await
    );
    assert!(!detects(salvo_core::http::Method::GET, "/api/v1/plugins").await);
}

#[tokio::test]
async fn extension_provider_materialization_has_a_dedicated_create_route() {
    let mut request = Request::new();
    *request.method_mut() = salvo_core::http::Method::POST;
    *request.uri_mut() = "http://local.test/api/v1/providers/from-extension"
        .parse()
        .unwrap();
    let root = api_router();
    let mut path = PathState::from_owned_path(request.uri().path().to_owned());
    assert!(root.detect(&mut request, &mut path).await.is_some());
}

fn api_token(html: &str) -> String {
    let marker = "window.__TERNILO_BOOT__ = ";
    let json = html
        .split_once(marker)
        .expect("boot marker")
        .1
        .split_once(";</script>")
        .expect("boot script end")
        .0;
    serde_json::from_str::<serde_json::Value>(json).unwrap()["apiToken"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn local_web_rejects_non_loopback_bind_addresses() {
    for address in ["0.0.0.0:0", "[::]:0", "192.0.2.1:0", "[2001:db8::1]:0"] {
        let error = bind_loopback(address.parse().unwrap()).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only binds to a loopback address")
        );
    }
}

#[tokio::test]
async fn local_web_rejects_untrusted_hosts_and_requires_the_boot_token() {
    let data_dir = tempfile::tempdir().unwrap();
    let application = crate::open_local_application(
        ternilo_local::local_profile(),
        data_dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    let listener = bind_loopback("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = {
        let application = Arc::clone(&application);
        tokio::spawn(async move { serve_application_on_listener(listener, application).await })
    };
    let client = reqwest::Client::new();
    let base = format!("http://{address}");
    let index_response = client
        .get(&base)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        index_response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let html = index_response.text().await.unwrap();
    assert!(html.contains("\"openConfig\":true"));
    let token = api_token(&html);

    assert_eq!(
        client
            .get(format!("{base}/api/v1/health"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/health"))
            .bearer_auth("wrong-token")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/health"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        client
            .get(&base)
            .header(reqwest::header::HOST, "attacker.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/health"))
            .header(reqwest::header::HOST, "attacker.invalid")
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/health"))
            .header(
                reqwest::header::HOST,
                format!("localhost:{}", address.port())
            )
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );

    server.abort();
    let _ = server.await;
    application.shutdown().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep setup, protocol actions and assertions together for this integration scenario."
)]
async fn fork_and_archive_routes_preserve_session_history() {
    let data_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let application = crate::open_local_application(
        ternilo_local::local_profile(),
        data_dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let source = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let source_id = source.identity.session_id.as_str().to_owned();
    application
        .run_turn(&source_id, None, "/code \"api first\"".to_owned())
        .await
        .unwrap();
    let first_turn = application.events(&source_id).await.unwrap();
    let first_terminal = first_turn
        .iter()
        .rposition(|event| {
            matches!(
                event.kind,
                ternilo_protocol::SessionEventKind::TurnFinished { .. }
                    | ternilo_protocol::SessionEventKind::TurnFailed { .. }
                    | ternilo_protocol::SessionEventKind::TurnCancelled
            )
        })
        .unwrap();
    application
        .run_turn(&source_id, None, "/code \"api second\"".to_owned())
        .await
        .unwrap();
    let source_events = application.events(&source_id).await.unwrap();

    let listener = bind_loopback("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = {
        let application = Arc::clone(&application);
        tokio::spawn(async move { serve_application_on_listener(listener, application).await })
    };
    let client = reqwest::Client::new();
    let base = format!("http://{address}");
    let html = client
        .get(&base)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    let token = api_token(&html);

    let response = client
        .post(format!("{base}/api/v1/sessions/{source_id}/fork"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "at_seq": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let anchored: LocalSession = response.json().await.unwrap();
    assert_eq!(
        application
            .events(anchored.identity.session_id.as_str())
            .await
            .unwrap(),
        source_events[..=first_terminal]
    );

    let session_count = application.snapshot().await.sessions.len();
    let response = client
        .post(format!("{base}/api/v1/sessions/{source_id}/fork"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "at_seq": source_events.last().unwrap().seq + 1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(application.snapshot().await.sessions.len(), session_count);

    let response = client
        .post(format!("{base}/api/v1/sessions/{source_id}/fork"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let latest: LocalSession = response.json().await.unwrap();
    assert_eq!(
        application
            .events(latest.identity.session_id.as_str())
            .await
            .unwrap(),
        source_events
    );

    let response = client
        .post(format!("{base}/api/v1/sessions/{source_id}/archive"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let archived: LocalSession = response.json().await.unwrap();
    assert!(archived.archived_at_ms.is_some());
    assert_eq!(application.events(&source_id).await.unwrap(), source_events);

    server.abort();
    let _ = server.await;
    application.shutdown().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep setup, protocol actions and assertions together for this integration scenario."
)]
async fn workspace_routes_rename_and_unregister_without_deleting_session_state() {
    let data_dir = tempfile::tempdir().unwrap();
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    tokio::fs::write(first_dir.path().join("kept.txt"), "kept")
        .await
        .unwrap();
    let application = crate::open_local_application(
        ternilo_local::local_profile(),
        data_dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    let first = application
        .add_workspace(first_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let second = application
        .add_workspace(second_dir.path().to_str().unwrap())
        .await
        .unwrap();
    application
        .rename_workspace(second.workspace_id, "occupied".to_owned())
        .await
        .unwrap();
    let session = application
        .create_session(first.workspace_id.clone(), None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str().to_owned();
    application
        .run_turn(&session_id, None, "/code \"retained log\"".to_owned())
        .await
        .unwrap();

    let listener = bind_loopback("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = {
        let application = Arc::clone(&application);
        tokio::spawn(async move { serve_application_on_listener(listener, application).await })
    };
    let client = reqwest::Client::new();
    let base = format!("http://{address}");
    let token = api_token(
        &client
            .get(&base)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap(),
    );
    let workspace_route = format!("{base}/api/v1/workspaces/{}", first.workspace_id.as_str());

    let response = client
        .patch(&workspace_route)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "title": "  renamed  " }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.json::<Workspace>().await.unwrap().title, "renamed");

    let conflict = client
        .patch(&workspace_route)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "title": "occupied" }))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), reqwest::StatusCode::BAD_REQUEST);

    let removed = client
        .delete(&workspace_route)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), reqwest::StatusCode::NO_CONTENT);
    let state = client
        .get(format!("{base}/api/v1/state"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(
        state["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .all(|workspace| {
                workspace["workspace_id"].as_str() != Some(first.workspace_id.as_str())
            })
    );
    let retained = state["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["identity"]["session_id"] == session_id)
        .unwrap();
    assert_eq!(
        retained["workspace_path"].as_str(),
        Some(first_dir.path().to_str().unwrap())
    );
    assert_eq!(
        tokio::fs::read_to_string(first_dir.path().join("kept.txt"))
            .await
            .unwrap(),
        "kept"
    );
    assert!(!application.events(&session_id).await.unwrap().is_empty());
    let resumed = application
        .run_turn(&session_id, None, "/code \"still runnable\"".to_owned())
        .await
        .unwrap();
    assert!(resumed.answer.contains("still runnable"));

    let unknown = client
        .delete(&workspace_route)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::BAD_REQUEST);
    server.abort();
    let _ = server.await;
    application.shutdown().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep setup, protocol actions and assertions together for this integration scenario."
)]
async fn agent_team_routes_persist_tasks_and_mailbox_with_revision_conflicts() {
    let data_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let application = crate::open_local_application(
        ternilo_local::local_profile(),
        data_dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();

    let listener = bind_loopback("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = {
        let application = Arc::clone(&application);
        tokio::spawn(async move { serve_application_on_listener(listener, application).await })
    };
    let client = reqwest::Client::new();
    let base = format!("http://{address}");
    let token = api_token(
        &client
            .get(&base)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap(),
    );
    let team_route = format!("{base}/api/v1/sessions/{session_id}/team");

    let initial = client
        .get(&team_route)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<ternilo_protocol::AgentTeamSnapshot>()
        .await
        .unwrap();
    assert_eq!(initial.members.len(), 1);
    let member_id = initial.current_member_id;

    let created = client
        .post(format!("{team_route}/tasks"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "subject": "Review Team API",
            "status": "in_progress",
            "owner": member_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let task = created
        .json::<ternilo_protocol::AgentTeamTask>()
        .await
        .unwrap();

    let conflict = client
        .put(format!("{team_route}/tasks/{}", task.id.as_str()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_revision": 99,
            "subject": task.subject,
            "status": "completed",
            "owner": member_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), reqwest::StatusCode::CONFLICT);

    let message = client
        .post(format!("{team_route}/messages"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "to": member_id,
            "content": "Team API ready",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(message.status(), reqwest::StatusCode::CREATED);
    let message = message
        .json::<ternilo_protocol::AgentTeamMessage>()
        .await
        .unwrap();
    let read = client
        .put(format!(
            "{team_route}/messages/{}/read",
            message.id.as_str()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<ternilo_protocol::AgentTeamMessage>()
        .await
        .unwrap();
    assert!(read.read_at_ms.is_some());

    let persisted = client
        .get(&team_route)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<ternilo_protocol::AgentTeamSnapshot>()
        .await
        .unwrap();
    assert_eq!(persisted.tasks.len(), 1);
    assert_eq!(persisted.messages.len(), 1);

    server.abort();
    let _ = server.await;
    application.shutdown().await.unwrap();
}
