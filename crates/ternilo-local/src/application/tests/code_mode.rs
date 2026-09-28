use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "hook bridge scenario verifies all five lifecycle points in one turn"
)]
async fn claude_hook_bridge_runs_the_five_agent_points_end_to_end() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let hook_config = workspace_dir.join("hooks.json");
    tokio::fs::write(workspace_dir.join("note.txt"), "hook-roundtrip")
        .await
        .unwrap();
    let config = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "startup",
                "hooks": [{
                    "command": "payload=$(cat); printf '%s' \"$payload\" > .hook-session.json; printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"SessionStart\",\"additionalContext\":\"session context\"}}'"
                }]
            }],
            "UserPromptSubmit": [{
                "matcher": "[ignored invalid matcher",
                "hooks": [{
                    "type": "command",
                    "command": "payload=$(cat); printf '%s' \"$payload\" > .hook-prompt.json; printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"UserPromptSubmit\",\"additionalContext\":\"prompt context\"}}'"
                }]
            }],
            "PreToolUse": [{
                "matcher": "read_file",
                "hooks": [{
                    "command": "payload=$(cat); printf '%s' \"$payload\" > .hook-pre.json; printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\"}}'"
                }]
            }],
            "PostToolUse": [{
                "matcher": "read_file",
                "hooks": [{
                    "type": "command",
                    "command": "payload=$(cat); printf '%s' \"$payload\" > .hook-post.json; printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PostToolUse\",\"additionalContext\":\"post context\"}}'"
                }]
            }],
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": "payload=$(cat); printf '%s' \"$payload\" > .hook-stop.json; printf '{}'"
                }]
            }]
        }
    });
    tokio::fs::write(&hook_config, serde_json::to_vec(&config).unwrap())
        .await
        .unwrap();

    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();
    application
        .update_profile_plugins(
            session_id,
            vec![PluginEntry {
                id: "claude-hooks".to_owned(),
                kind: ternilo_builtins::CLAUDE_CODE_HOOKS_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({
                    "configPath": hook_config,
                    "projectDir": workspace_dir,
                    "stderrSummaryMaxChars": 500,
                }),
            }],
        )
        .await
        .unwrap();
    let outcome = application
        .run_turn(
            session_id,
            Some("hook-run".to_owned()),
            "/read note.txt".to_owned(),
        )
        .await
        .unwrap();
    assert!(!outcome.answer.is_empty());

    let read_payload = |name: &str| {
        let bytes = std::fs::read(workspace_dir.join(name)).unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    };
    let session_payload = read_payload(".hook-session.json");
    assert_eq!(session_payload["source"], "startup");
    assert_eq!(session_payload["hook_event_name"], "SessionStart");
    let prompt_payload = read_payload(".hook-prompt.json");
    assert_eq!(prompt_payload["prompt"], "/read note.txt");
    let pre_payload = read_payload(".hook-pre.json");
    assert_eq!(pre_payload["tool_name"], "read_file");
    assert_eq!(pre_payload["tool_input"]["path"], "note.txt");
    let post_payload = read_payload(".hook-post.json");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(post_payload["tool_response"].as_str().unwrap())
            .unwrap()["content"],
        "hook-roundtrip"
    );
    let stop_payload = read_payload(".hook-stop.json");
    assert_eq!(stop_payload["stop_hook_active"], false);

    let events = application.events(session_id).await.unwrap();
    let hook_results = events
        .iter()
        .filter(|event| matches!(event.kind, SessionEventKind::HookResult { .. }))
        .count();
    assert_eq!(hook_results, 5);
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::HookContextAdded { content, .. } if content == "post context"
    )));

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn code_mode_composes_native_tools_and_records_nested_dispatches() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    tokio::fs::write(workspace_dir.join("note.txt"), "from-code")
        .await
        .unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();

    let outcome = application
        .run_turn(
            session_id,
            Some("code-mode-run".to_owned()),
            r#"/code let answer = tools::read_file(#{ path: "note.txt" }); print(answer); #{ nested: answer }"#
                .to_owned(),
        )
        .await
        .unwrap();
    assert!(outcome.answer.contains("from-code"), "{}", outcome.answer);

    let events = application.events(session_id).await.unwrap();
    let (parent_call_id, nested_call_id) = events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::CodeDispatchStarted {
                parent_call_id,
                call,
            } if call.name == "read_file" && call.arguments["path"] == "note.txt" => {
                Some((parent_call_id.clone(), call.id.clone()))
            }
            _ => None,
        })
        .expect("nested file read dispatch was recorded");
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::ToolCallStarted { call }
            if call.id == parent_call_id && call.name == "run_code"
    )));
    assert!(nested_call_id.starts_with(&format!("{parent_call_id}:code:")));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::CodeDispatchFinished {
            parent_call_id: finished_parent,
            call_id,
            name,
            output,
            ..
        } if finished_parent == &parent_call_id
            && call_id == &nested_call_id
            && name == "read_file"
            && !output.is_error
            && serde_json::from_str::<serde_json::Value>(&output.content).unwrap()["content"] == "from-code"
    )));
    assert_eq!(application.stats(session_id).await.unwrap().tool_calls, 2);

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn code_only_presentation_exposes_transport_and_keeps_native_bindings_nested() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    tokio::fs::write(workspace_dir.join("note.txt"), "nested-visible")
        .await
        .unwrap();
    let application = Arc::new(open_test_application(data_dir.clone()).await);
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();
    application
        .update_profile_plugins(
            session_id,
            vec![PluginEntry {
                id: "code-mode".to_owned(),
                kind: ternilo_code_runtime::CODE_MODE_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({ "mode": "code" }),
            }],
        )
        .await
        .unwrap();

    let direct = application
        .run_turn(
            session_id,
            Some("direct-hidden".to_owned()),
            "/read note.txt".to_owned(),
        )
        .await
        .unwrap();
    assert!(
        direct
            .answer
            .contains("current code-only tool presentation")
    );
    assert!(direct.events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::CommandFinished { outcome, .. }
            if outcome.kind == SessionCommandOutcomeKind::Error
    )));
    let nested = application
        .run_turn(
            session_id,
            Some("nested-visible".to_owned()),
            r#"/code tools::read_file(#{ path: "note.txt" })"#.to_owned(),
        )
        .await
        .unwrap();
    assert!(nested.answer.contains("nested-visible"));
    let events = application.events(session_id).await.unwrap();
    assert!(events.iter().any(|event| {
        event.run_id.as_str() == "direct-hidden"
            && matches!(
                &event.kind,
                SessionEventKind::ToolCallFinished { name, output, .. }
                    if name == "read_file" && output.is_error
            )
    }));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::CodeDispatchStarted { call, .. }
            if call.name == "read_file" && call.arguments["path"] == "note.txt"
    )));

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn code_mode_nested_calls_pass_through_hooks_and_preserve_denials() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let hook_config = workspace_dir.join("deny-read-hooks.json");
    let config = serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "read_file",
                "hooks": [{
                    "command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"nested read denied\"}}'"
                }]
            }]
        }
    });
    tokio::fs::write(&hook_config, serde_json::to_vec(&config).unwrap())
        .await
        .unwrap();

    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();
    application
        .update_profile_plugins(
            session_id,
            vec![PluginEntry {
                id: "deny-read-hooks".to_owned(),
                kind: ternilo_builtins::CLAUDE_CODE_HOOKS_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({
                    "configPath": hook_config,
                    "projectDir": workspace_dir,
                }),
            }],
        )
        .await
        .unwrap();

    let outcome = application
        .run_turn(
            session_id,
            Some("code-hook-denial".to_owned()),
            r#"/code call_tool("read_file", #{ path: "must-not-run.txt" })"#.to_owned(),
        )
        .await
        .unwrap();
    assert!(
        outcome.answer.contains("nested read denied"),
        "{}",
        outcome.answer
    );
    let events = application.events(session_id).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::HookResult { result }
            if result.point == ternilo_protocol::HookPoint::PreToolUse
                && result.decision == ternilo_protocol::HookDecision::Deny
                && result.reason.as_deref() == Some("nested read denied")
    )));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::CodeDispatchFinished { name, output, .. }
            if name == "read_file"
                && output.is_error
                && output.content.contains("nested read denied")
    )));
    assert!(!events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::CodeDispatchFinished { name, output, .. }
            if name == "read_file" && !output.is_error
    )));

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the integration scenario starts, cancels, and reuses one code runtime"
)]
async fn cancelling_a_code_mode_hot_loop_releases_the_turn() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = Arc::new(open_test_application(data_dir.clone()).await);
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str().to_owned();
    application
        .update_profile_plugins(
            &session_id,
            vec![PluginEntry {
                id: "rhai-code-runtime".to_owned(),
                kind: ternilo_code_runtime::RUNTIME_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({
                    "max_operations": 1_000_000_000_000_u64,
                    "max_wall_ms": 30_000,
                }),
            }],
        )
        .await
        .unwrap();
    let run_id = "cancel-code-hot-loop".to_owned();
    let running = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        let run_id = run_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(&session_id, Some(run_id), "/code loop { }".to_owned())
                .await
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if application
                .events(&session_id)
                .await
                .unwrap()
                .iter()
                .any(|event| {
                    matches!(
                        &event.kind,
                        SessionEventKind::ToolCallStarted { call } if call.name == "run_code"
                    )
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    application.cancel_turn(&session_id, &run_id).await.unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), running)
        .await
        .expect("cancelled code turn did not quiesce")
        .unwrap()
        .unwrap_err();
    assert!(error.is_cancelled(), "{error}");
    assert!(
        application
            .events(&session_id)
            .await
            .unwrap()
            .iter()
            .any(|event| {
                event.run_id.as_str() == run_id
                    && matches!(event.kind, SessionEventKind::TurnCancelled)
            })
    );
    assert!(
        application
            .events(&session_id)
            .await
            .unwrap()
            .iter()
            .any(|event| {
                event.run_id.as_str() == run_id
                    && matches!(
                        &event.kind,
                        SessionEventKind::ToolCallFinished { name, output, .. }
                            if name == "run_code"
                                && output.is_error
                                && output.content == "tool_call_cancelled"
                    )
            })
    );
    let next = application
        .run_turn(
            &session_id,
            None,
            "/code \"still usable after code cancellation\"".to_owned(),
        )
        .await
        .unwrap();
    assert!(next.answer.contains("still usable after code cancellation"));

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
