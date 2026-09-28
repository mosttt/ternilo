use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise persistent Subagent creation, execution and cancellation through the real parent protocol."
)]
async fn controlled_child_uses_parent_host_for_persistent_subagent_tools() {
    let (_workspace, mut child, mut input, mut output) =
        spawn_controlled_child("controlled-subagent-run", "delegate this task").await;
    let first = next_frame(&mut output, "model_request").await;
    let tools = first["request"]["tools"].as_array().unwrap();
    for name in [
        "spawn_agent",
        "send_agent_message",
        "list_agents",
        "wait_agent",
        "interrupt_agent",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == name),
            "missing {name}"
        );
    }
    write_model_tool_response(
        &mut input,
        first["request_id"].as_u64().unwrap(),
        "spawn-1",
        "spawn_agent",
        json!({ "task": "inspect the workspace", "background": false }),
    )
    .await;
    let create = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(create["request"]["operation"], "create_subagent");
    let subagent_id = create["request"]["subagent_id"]
        .as_str()
        .unwrap()
        .to_owned();
    write_frame(
        &mut input,
        json!({
            "type": "host_reply",
            "request_id": create["request_id"],
            "outcome": { "status": "ok", "value": "controlled-child-session" },
        }),
    )
    .await;
    let initial_run = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(initial_run["request"]["operation"], "enqueue_subagent");
    let initial_ticket = write_host_admission(&mut input, &initial_run).await;
    complete_scheduled_subagent(
        &mut input,
        &mut output,
        &initial_ticket,
        "first delegated answer",
    )
    .await;

    let second = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    assert!(
        second["request"]["messages"]
            .to_string()
            .contains("first delegated answer")
    );
    write_model_tool_response(
        &mut input,
        second["request_id"].as_u64().unwrap(),
        "send-1",
        "send_agent_message",
        json!({ "subagent_id": subagent_id, "message": "review once more" }),
    )
    .await;
    let followup_run =
        next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(followup_run["request"]["operation"], "enqueue_subagent");
    assert_eq!(followup_run["request"]["input"], "review once more");
    assert_no_subagent_completion(&mut output).await;
    let followup_ticket = write_host_admission(&mut input, &followup_run).await;
    let mut third = None;
    let mut followup_wait = None;
    while third.is_none() || followup_wait.is_none() {
        let frame = next_non_error_frame(&mut output).await;
        match frame["type"].as_str() {
            Some("model_request") => third = Some(frame),
            Some("host_request") => {
                assert_eq!(frame["request"]["operation"], "wait_subagent");
                assert_eq!(frame["request"]["run_id"], followup_ticket["run_id"]);
                followup_wait = Some(frame);
            }
            _ => {}
        }
    }
    let third = third.unwrap();
    write_model_tool_response(
        &mut input,
        third["request_id"].as_u64().unwrap(),
        "interrupt-1",
        "interrupt_agent",
        json!({ "subagent_id": subagent_id }),
    )
    .await;
    let cancel = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(cancel["request"]["operation"], "cancel_subagent");
    assert_eq!(cancel["request"]["session_id"], "controlled-child-session");
    assert_eq!(
        cancel["request"]["run_id"],
        followup_run["request"]["run_id"]
    );
    write_host_value(&mut input, &cancel, json!("cancelled")).await;

    let fourth = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    assert!(
        fourth["request"]["messages"]
            .to_string()
            .contains("cancelled")
    );
    write_model_tool_response(
        &mut input,
        fourth["request_id"].as_u64().unwrap(),
        "wait-1",
        "wait_agent",
        json!({ "subagent_id": subagent_id, "timeout_ms": 1_000 }),
    )
    .await;
    let fifth = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    assert!(
        fifth["request"]["messages"]
            .to_string()
            .contains("cancelled")
    );
    write_model_tool_response(
        &mut input,
        fifth["request_id"].as_u64().unwrap(),
        "list-1",
        "list_agents",
        json!({}),
    )
    .await;
    let sixth = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    assert!(
        sixth["request"]["messages"]
            .to_string()
            .contains("cancelled")
    );
    write_model_response(
        &mut input,
        sixth["request_id"].as_u64().unwrap(),
        "delegation complete",
    )
    .await;

    loop {
        let frame = next_non_error_frame(&mut output).await;
        match frame["type"].as_str() {
            Some("model_request") => {
                write_model_response(
                    &mut input,
                    frame["request_id"].as_u64().unwrap(),
                    "Controlled Subagent title",
                )
                .await;
            }
            Some("outcome") => {
                assert_eq!(frame["outcome"]["answer"], "delegation complete");
                break;
            }
            _ => {}
        }
    }
    drop(input);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("Subagent child did not exit")
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn controlled_child_reports_rejected_admission_before_background_spawn_succeeds() {
    let (_workspace, mut child, mut input, mut output) =
        spawn_controlled_child("rejected-subagent-run", "delegate if admitted").await;
    let first = next_frame(&mut output, "model_request").await;
    write_model_tool_response(
        &mut input,
        first["request_id"].as_u64().unwrap(),
        "rejected-spawn",
        "spawn_agent",
        json!({ "task": "must be admitted first", "background": true }),
    )
    .await;
    let create = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(create["request"]["operation"], "create_subagent");
    write_host_value(&mut input, &create, json!("rejected-child-session")).await;
    let admission = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(admission["request"]["operation"], "enqueue_subagent");
    assert_no_subagent_completion(&mut output).await;
    write_frame(&mut input, json!({
        "type": "host_reply", "request_id": admission["request_id"],
        "outcome": { "status": "error", "error": ternilo_protocol::HarnessError::policy("Subagent admission rejected by Server") },
    })).await;
    let next = loop {
        let frame = next_non_error_frame(&mut output).await;
        assert_ne!(
            frame["type"], "host_request",
            "rejected admission must not start waiting for a run"
        );
        if frame["type"] == "model_request" {
            break frame;
        }
    };
    assert!(
        next["request"]["messages"]
            .to_string()
            .contains("Subagent admission rejected by Server")
    );
    write_model_response(
        &mut input,
        next["request_id"].as_u64().unwrap(),
        "admission failed clearly",
    )
    .await;
    loop {
        let frame = next_non_error_frame(&mut output).await;
        match frame["type"].as_str() {
            Some("model_request") => {
                write_model_response(
                    &mut input,
                    frame["request_id"].as_u64().unwrap(),
                    "Rejected admission",
                )
                .await;
            }
            Some("outcome") => {
                assert_eq!(frame["outcome"]["answer"], "admission failed clearly");
                break;
            }
            _ => {}
        }
    }
    drop(input);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn controlled_child_waits_for_subagent_cleanup_confirmation_before_reporting_cancellation() {
    let (_workspace, mut child, mut input, mut output) =
        spawn_controlled_child("cleanup-subagent-run", "delegate then finish").await;
    let (admission, _next_model) = accept_background_subagent(&mut input, &mut output).await;
    write_frame(
        &mut input,
        json!({
            "type": "session_command", "command_id": "cancel-parent",
            "command": { "operation": "cancel", "run_id": "cleanup-subagent-run" },
        }),
    )
    .await;
    let cancel = loop {
        let frame = next_non_error_frame(&mut output).await;
        match frame["type"].as_str() {
            Some("model_request") => {
                write_model_response(
                    &mut input,
                    frame["request_id"].as_u64().unwrap(),
                    "Cleanup order",
                )
                .await;
            }
            Some("host_request") => {
                assert_eq!(frame["request"]["operation"], "cancel_subagent");
                break frame;
            }
            Some("outcome") => panic!("parent outcome preceded background cancellation: {frame}"),
            _ => {}
        }
    };
    assert_eq!(cancel["request"]["session_id"], "cleanup-child-session");
    assert_eq!(cancel["request"]["run_id"], admission["request"]["run_id"]);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
    while let Ok(line) = tokio::time::timeout_at(deadline, output.next_line()).await {
        let frame: Value = serde_json::from_str(
            &line
                .unwrap()
                .expect("child exited before cleanup confirmation"),
        )
        .unwrap();
        assert_ne!(
            frame["type"], "outcome",
            "final outcome must await the real cancellation reply"
        );
        assert_ne!(
            frame["type"], "error",
            "cleanup must not fail before the reply"
        );
    }
    write_host_value(&mut input, &cancel, json!("cancelled")).await;
    let error = next_frame(&mut output, "error").await;
    assert_eq!(error["error"]["code"], "cancelled");
    drop(input);
    assert!(
        !tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn a_successful_parent_keeps_the_delivered_background_run_accepted_and_running() {
    let (_workspace, mut child, mut input, mut output) =
        spawn_controlled_child("preserved-subagent-run", "delegate then finish").await;
    let (_admission, next_model) = accept_background_subagent(&mut input, &mut output).await;
    write_model_response(
        &mut input,
        next_model["request_id"].as_u64().unwrap(),
        "parent finished",
    )
    .await;
    loop {
        let frame = next_non_error_frame(&mut output).await;
        assert_ne!(
            frame["type"], "host_request",
            "successful parent shutdown must not cancel an accepted background run"
        );
        if frame["event"]["type"] == "subagent_updated" {
            assert_ne!(frame["event"]["subagent"]["status"], "cancelled");
        }
        match frame["type"].as_str() {
            Some("model_request") => {
                write_model_response(
                    &mut input,
                    frame["request_id"].as_u64().unwrap(),
                    "Background retained",
                )
                .await;
            }
            Some("outcome") => {
                assert_eq!(frame["outcome"]["answer"], "parent finished");
                break;
            }
            _ => {}
        }
    }
    drop(input);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

async fn accept_background_subagent(
    input: &mut ChildStdin,
    output: &mut ChildOutput,
) -> (Value, Value) {
    let first = next_frame(output, "model_request").await;
    write_model_tool_response(
        input,
        first["request_id"].as_u64().unwrap(),
        "background-spawn",
        "spawn_agent",
        json!({ "task": "continue in the background", "background": true }),
    )
    .await;
    let create = next_frame_answering_questions(input, output, "host_request").await;
    write_host_value(input, &create, json!("cleanup-child-session")).await;
    let admission = next_frame_answering_questions(input, output, "host_request").await;
    assert_eq!(admission["request"]["operation"], "enqueue_subagent");
    let ticket = write_host_admission(input, &admission).await;
    let mut next_model = None;
    let mut waiting = false;
    while next_model.is_none() || !waiting {
        let frame = next_non_error_frame(output).await;
        match frame["type"].as_str() {
            Some("model_request") => next_model = Some(frame),
            Some("host_request") => {
                assert_eq!(frame["request"]["operation"], "wait_subagent");
                assert_eq!(frame["request"]["run_id"], ticket["run_id"]);
                waiting = true;
            }
            _ => {}
        }
    }
    (admission, next_model.unwrap())
}
