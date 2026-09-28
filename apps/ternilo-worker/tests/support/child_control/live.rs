use super::*;

#[tokio::test]
async fn controlled_child_batches_separate_user_messages_before_the_first_model_call() {
    let submission_id = SubmissionId::new("batch-C");
    let additional = SteeringInput {
        submission_id: submission_id.clone(),
        input: "C: second requirement".to_owned(),
        provenance: Some(ternilo_protocol::InputProvenance {
            run_id: None,
            input_id: submission_id.clone(),
            author: ternilo_protocol::InputAuthor::Account {
                user_id: UserId::new("second-author"),
                username: "teammate".to_owned(),
            },
        }),
        display_input: None,
        source: UserMessageSource::Submission {
            submission_id,
            created_at_ms: 1,
            delivery: SubmissionDelivery::Queue,
            regenerate_from: None,
            skill_name: None,
        },
        references: Vec::new(),
        reference_contexts: Vec::new(),
        attachments: Vec::new(),
    };
    let (_workspace, mut child, mut input, mut output) = spawn_controlled_child_with_batch(
        "controlled-batch",
        "B: first requirement",
        false,
        SessionMode::Execute,
        true,
        vec![additional],
    )
    .await;
    let (request, inputs) = next_model_request_with_inputs(&mut output).await;
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0]["content"], "B: first requirement");
    assert_eq!(inputs[1]["content"], "C: second requirement");
    assert_eq!(inputs[0]["run_id"], inputs[1]["run_id"]);
    assert_eq!(
        inputs[0]["provenance"]["author"]["user_id"],
        "controlled-user"
    );
    assert_eq!(
        inputs[1]["provenance"]["author"]["user_id"],
        "second-author"
    );
    let users = request["request"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["content"], "B: first requirement");
    assert_eq!(users[1]["content"], "C: second requirement");
    assert_eq!(request["request"]["step"], 1);
    write_model_response(
        &mut input,
        request["request_id"].as_u64().unwrap(),
        "Batch answer",
    )
    .await;
    loop {
        let frame = next_non_error_frame(&mut output).await;
        if frame["type"] == "model_request" {
            write_model_response(
                &mut input,
                frame["request_id"].as_u64().unwrap(),
                "Batch title",
            )
            .await;
        } else if frame["type"] == "outcome" {
            assert_eq!(frame["outcome"]["answer"], "Batch answer");
            break;
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
async fn a_parent_disappearing_before_authorization_cannot_boot_the_workspace() {
    let (workspace, mut child, mut input, mut output) =
        spawn_controlled_child_with_workspace_skill_and_mode(
            "unauthorized-startup",
            "must not execute",
            false,
            SessionMode::Execute,
            false,
        )
        .await;
    assert!(!workspace.path().join(".ternilo").exists());
    input.shutdown().await.unwrap();
    drop(input);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    assert!(output.next_line().await.unwrap().is_none());
    assert!(
        !workspace.path().join(".ternilo").exists(),
        "parent EOF must not initialize attachments, plugins, or the workspace"
    );
    let mut error = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut error)
        .await
        .unwrap();
    assert!(error.contains("startup authorization"), "{error}");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify initial and steered input authors across one real controlled Worker child lifecycle."
)]
async fn controlled_child_provider_receives_steering_on_the_next_step() {
    let (_workspace, mut child, mut input, mut output) =
        spawn_controlled_child("controlled-steer-run", "first request").await;

    let (first_request, initial_inputs) = next_model_request_with_inputs(&mut output).await;
    assert_eq!(initial_inputs.len(), 1);
    assert_eq!(
        initial_inputs[0]["provenance"],
        json!({
            "input_id":"controlled-steer-run-input",
            "author":{"kind":"account","user_id":"controlled-user","username":"controlled-user"}
        })
    );
    assert!(
        initial_inputs[0].get("source").is_none(),
        "direct execution retains provenance without queue metadata"
    );
    let first_request_id = first_request["request_id"].as_u64().unwrap();
    let submission_id = SubmissionId::new("controlled-steer");
    let steering = SteeringInput {
        provenance: Some(ternilo_protocol::InputProvenance {
            run_id: None,
            input_id: submission_id.clone(),
            author: ternilo_protocol::InputAuthor::Account {
                user_id: UserId::new("steering-user"),
                username: "steering-user".to_owned(),
            },
        }),
        submission_id: submission_id.clone(),
        input: "steered at the live boundary".to_owned(),
        display_input: None,
        source: UserMessageSource::Submission {
            regenerate_from: None,
            submission_id,
            created_at_ms: 1,
            delivery: SubmissionDelivery::Steer,
            skill_name: None,
        },
        references: Vec::new(),
        reference_contexts: Vec::new(),
        attachments: Vec::new(),
    };
    write_frame(
        &mut input,
        json!({
            "type": "session_command",
            "command_id": CommandId::new("controlled-steer-command"),
            "command": {
                "operation": "steer",
                "input": steering,
            },
        }),
    )
    .await;
    let reply = next_frame(&mut output, "session_command_reply").await;
    assert_eq!(reply["command_id"], "controlled-steer-command");
    assert_eq!(
        reply["outcome"],
        json!({ "status": "steer", "accepted": true })
    );

    write_model_response(&mut input, first_request_id, "first answer").await;
    let (second_request, steered_inputs) = next_model_request_with_inputs(&mut output).await;
    assert_eq!(steered_inputs.len(), 1);
    assert_eq!(
        steered_inputs[0]["provenance"],
        json!({
            "input_id":"controlled-steer",
            "author":{"kind":"account","user_id":"steering-user","username":"steering-user"}
        })
    );
    assert_eq!(
        second_request["request"]["step"], 2,
        "unexpected second model request: {second_request}",
    );
    assert!(
        second_request["request"]["messages"]
            .to_string()
            .contains("steered at the live boundary"),
        "the controlled Provider's second request must contain the accepted steering input",
    );
    write_model_response(
        &mut input,
        second_request["request_id"].as_u64().unwrap(),
        "second answer",
    )
    .await;

    let mut outcome = None;
    while outcome.is_none() {
        let frame = tokio::time::timeout(Duration::from_secs(5), output.next_line())
            .await
            .expect("child protocol timed out")
            .unwrap()
            .expect("child closed before outcome");
        let frame: Value = serde_json::from_str(&frame).unwrap();
        match frame["type"].as_str() {
            Some("model_request") => {
                write_model_response(
                    &mut input,
                    frame["request_id"].as_u64().unwrap(),
                    "Controlled session title",
                )
                .await;
            }
            Some("outcome") => outcome = Some(frame),
            Some("error") => panic!("child returned an error: {frame}"),
            _ => {}
        }
    }
    assert_eq!(outcome.unwrap()["outcome"]["answer"], "second answer");
    drop(input);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("child did not exit")
        .unwrap();
    if !status.success() {
        let mut stderr = child.stderr.take().expect("child stderr");
        let mut text = String::new();
        stderr.read_to_string(&mut text).await.unwrap();
        panic!("controlled child exited with {status}: {text}");
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep workspace Skill resolution and the provider payload in one controlled child scenario."
)]
async fn controlled_child_resolves_workspace_skill_before_the_provider_receives_it() {
    let (_workspace, mut child, mut input, mut output) =
        spawn_controlled_child_with_workspace_skill(
            "controlled-skill-run",
            "open the skill boundary",
            true,
        )
        .await;
    let first_request = next_frame(&mut output, "model_request").await;
    write_frame(
        &mut input,
        json!({
            "type": "session_command",
            "command_id": CommandId::new("controlled-skill-resolve"),
            "command": {
                "operation": "resolve_skill",
                "name": "release-check",
                "input": "inspect the artifact",
            },
        }),
    )
    .await;
    let reply = next_frame(&mut output, "session_command_reply").await;
    let invocation: PreparedSkillInvocation =
        serde_json::from_value(reply["outcome"]["invocation"].clone()).unwrap();
    assert_eq!(reply["outcome"]["status"], "skill_resolved");
    assert!(
        invocation
            .model_input
            .contains("Verify the controlled provider payload.")
    );
    assert_eq!(
        invocation.display_input,
        "/skill release-check\n\ninspect the artifact"
    );

    let submission_id = SubmissionId::new("controlled-skill-submission");
    write_frame(
        &mut input,
        json!({
            "type": "session_command",
            "command_id": CommandId::new("controlled-skill-steer"),
            "command": {
                "operation": "steer",
                "input": SteeringInput {
        provenance: None,
                    submission_id: submission_id.clone(),
                    input: invocation.model_input.clone(),
                    display_input: Some(invocation.display_input.clone()),
                    source: UserMessageSource::Submission {
                        regenerate_from: None,
                        submission_id,
                        created_at_ms: 1,
                        delivery: SubmissionDelivery::Steer,
                        skill_name: Some(invocation.name.clone()),
                    },
                    references: Vec::new(),
                    reference_contexts: Vec::new(),
                    attachments: Vec::new(),
                },
            },
        }),
    )
    .await;
    let steer = next_frame(&mut output, "session_command_reply").await;
    assert_eq!(
        steer["outcome"],
        json!({ "status": "steer", "accepted": true })
    );
    write_model_response(
        &mut input,
        first_request["request_id"].as_u64().unwrap(),
        "first answer",
    )
    .await;
    let second_request = next_frame(&mut output, "model_request").await;
    let provider_messages = second_request["request"]["messages"].to_string();
    assert!(provider_messages.contains("skill_content"));
    assert!(provider_messages.contains("Verify the controlled provider payload."));
    assert!(provider_messages.contains("inspect the artifact"));
    write_model_response(
        &mut input,
        second_request["request_id"].as_u64().unwrap(),
        "skill applied",
    )
    .await;

    loop {
        let frame = next_non_error_frame(&mut output).await;
        match frame["type"].as_str() {
            Some("model_request") => {
                write_model_response(
                    &mut input,
                    frame["request_id"].as_u64().unwrap(),
                    "Controlled skill title",
                )
                .await;
            }
            Some("outcome") => {
                assert_eq!(frame["outcome"]["answer"], "skill applied");
                break;
            }
            _ => {}
        }
    }
    drop(input);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("skill child did not exit")
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn controlled_child_uses_run_spec_mode_instead_of_inferring_it_from_permissions() {
    for (mode, expects_plan_review) in [(SessionMode::Execute, false), (SessionMode::Plan, true)] {
        let run_id = match mode {
            SessionMode::Execute => "controlled-execute-mode-run",
            SessionMode::Plan => "controlled-plan-mode-run",
        };
        let (_workspace, mut child, mut input, mut output) =
            spawn_controlled_child_in_mode(run_id, "review this plan", mode).await;
        let first = next_frame(&mut output, "model_request").await;
        write_model_tool_response(
            &mut input,
            first["request_id"].as_u64().unwrap(),
            "exit-plan-1",
            "exit_plan_mode",
            json!({ "plan": "# Controlled plan\n\nComplete the requested work." }),
        )
        .await;

        let second = if expects_plan_review {
            let question = next_frame(&mut output, "question").await;
            assert_eq!(question["question"]["presentation"]["kind"], "plan_review");
            write_frame(
                &mut input,
                json!({
                    "type": "question_answer",
                    "answer": {
                        "question_id": question["question"]["id"],
                        "selected": ["Keep planning"],
                    },
                }),
            )
            .await;
            next_frame(&mut output, "model_request").await
        } else {
            let request = next_frame(&mut output, "model_request").await;
            assert!(
                request["request"]["messages"]
                    .to_string()
                    .contains("exit_plan_mode may only be used while plan mode is active")
            );
            request
        };
        write_model_response(
            &mut input,
            second["request_id"].as_u64().unwrap(),
            "mode verified",
        )
        .await;

        loop {
            let frame = next_non_error_frame(&mut output).await;
            match frame["type"].as_str() {
                Some("model_request") => {
                    write_model_response(
                        &mut input,
                        frame["request_id"].as_u64().unwrap(),
                        "Controlled mode title",
                    )
                    .await;
                }
                Some("outcome") => {
                    assert_eq!(frame["outcome"]["answer"], "mode verified");
                    break;
                }
                _ => {}
            }
        }
        drop(input);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .expect("mode child did not exit")
                .unwrap()
                .success()
        );
    }
}

#[tokio::test]
async fn cancel_command_stops_a_child_blocked_on_the_controlled_provider() {
    let (_workspace, mut child, mut input, mut output) =
        spawn_controlled_child("controlled-cancel-run", "wait for cancellation").await;
    let _request = next_frame(&mut output, "model_request").await;
    for (command_id, command) in [
        ("list-live-services", json!({"operation":"services"})),
        (
            "start-missing-service",
            json!({"operation":"start_service","service_id":"missing"}),
        ),
        (
            "stop-missing-service",
            json!({"operation":"stop_service","service_id":"missing"}),
        ),
    ] {
        write_frame(
            &mut input,
            json!({"type":"session_command","command_id":command_id,"command":command}),
        )
        .await;
        let reply = next_frame(&mut output, "session_command_reply").await;
        assert_eq!(reply["command_id"], command_id);
        if command_id == "list-live-services" {
            assert_eq!(reply["outcome"], json!({"status":"services","services":[]}));
        } else {
            assert_eq!(
                reply["outcome"]["status"], "error",
                "unknown service must not fabricate a successful control result"
            );
        }
    }
    let started = tokio::time::Instant::now();
    write_frame(
        &mut input,
        json!({
            "type": "session_command",
            "command_id": CommandId::new("controlled-cancel-command"),
            "command": {
                "operation": "cancel",
                "run_id": RunId::new("controlled-cancel-run"),
            },
        }),
    )
    .await;
    let reply = next_frame(&mut output, "session_command_reply").await;
    assert_eq!(reply["command_id"], "controlled-cancel-command");
    assert_eq!(reply["outcome"], json!({ "status": "cancelled" }));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cancel must wake the blocked child without waiting for a run-lease renewal",
    );
    let error = next_frame(&mut output, "error").await;
    assert_eq!(error["error"]["code"], "cancelled");
    drop(input);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("cancelled child did not exit")
        .unwrap();
    assert!(
        !status.success(),
        "execute reports the cancelled run to its parent"
    );
}
