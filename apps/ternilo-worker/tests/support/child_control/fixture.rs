use super::*;

pub(super) type ChildOutput = tokio::io::Lines<BufReader<tokio::process::ChildStdout>>;

pub(super) async fn spawn_controlled_child(
    run_id: &str,
    prompt: &str,
) -> (tempfile::TempDir, Child, ChildStdin, ChildOutput) {
    spawn_controlled_child_in_mode(run_id, prompt, SessionMode::Execute).await
}

pub(super) async fn spawn_controlled_child_in_mode(
    run_id: &str,
    prompt: &str,
    mode: SessionMode,
) -> (tempfile::TempDir, Child, ChildStdin, ChildOutput) {
    spawn_controlled_child_with_workspace_skill_and_mode(run_id, prompt, false, mode, true).await
}

pub(super) async fn spawn_controlled_child_with_workspace_skill(
    run_id: &str,
    prompt: &str,
    install_skill: bool,
) -> (tempfile::TempDir, Child, ChildStdin, ChildOutput) {
    spawn_controlled_child_with_workspace_skill_and_mode(
        run_id,
        prompt,
        install_skill,
        SessionMode::Execute,
        true,
    )
    .await
}

pub(super) async fn spawn_controlled_child_with_workspace_skill_and_mode(
    run_id: &str,
    prompt: &str,
    install_skill: bool,
    mode: SessionMode,
    authorize: bool,
) -> (tempfile::TempDir, Child, ChildStdin, ChildOutput) {
    spawn_controlled_child_with_batch(run_id, prompt, install_skill, mode, authorize, Vec::new())
        .await
}

pub(super) async fn spawn_controlled_child_with_batch(
    run_id: &str,
    prompt: &str,
    install_skill: bool,
    mode: SessionMode,
    authorize: bool,
    additional_inputs: Vec<SteeringInput>,
) -> (tempfile::TempDir, Child, ChildStdin, ChildOutput) {
    let policy_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/docker/worker-policy.json");
    let policy: WorkerPolicy =
        serde_json::from_slice(&tokio::fs::read(&policy_path).await.unwrap()).unwrap();
    let catalog = ternilo_cloud::catalog().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    if install_skill {
        install_workspace_skill(workspace.path()).await;
    }
    let workspace_id = WorkspaceId::new(format!("{run_id}-workspace"));
    let compiled = policy
        .compile_run(
            CloudRunDraft {
                project_id: "controlled-project".to_owned(),
                workspace_id: workspace_id.clone(),
                agent_id: AgentId::new("controlled-agent"),
                session_id: SessionId::new(format!("{run_id}-session")),
                run_id: Some(RunId::new(run_id)),
                limits: RunLimits {
                    max_steps: 12,
                    max_tool_calls: 12,
                },
                permissions: PermissionPreset::WorkspaceWrite,
                mode,
                profile: ternilo_cloud::cloud_profile(Some(&ternilo_protocol::RunModelSnapshot {
                    binding: ternilo_protocol::RunModelBinding::Platform {
                        grant_id: "test-grant".to_owned(),
                        model_id: "test-model".to_owned(),
                        beneficiary_user_id: UserId::new("controlled-user"),
                    },
                    protocol: ternilo_protocol::ProviderProtocol::OpenAiChatCompletions,
                    defaults: ternilo_protocol::ProviderModelDefaults {
                        context_window: 128_000,
                        max_output_tokens: 4_096,
                        reasoning: None,
                    },
                    reasoning_effort: None,
                    display_name: "Test model".to_owned(),
                    source_name: "Test allowance".to_owned(),
                })),
                input: prompt.to_owned(),
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
                reserved_model_tokens: 1_000,
            },
            TenantId::new("controlled-tenant"),
            UserId::new("controlled-user"),
            UserId::new("controlled-user"),
            &catalog,
        )
        .unwrap();
    let envelope = ExecutionEnvelope {
        additional_inputs,
        provenance: Some(ternilo_protocol::InputProvenance {
            run_id: None,
            input_id: SubmissionId::new(format!("{run_id}-input")),
            author: ternilo_protocol::InputAuthor::Account {
                user_id: UserId::new("controlled-user"),
                username: "controlled-user".to_owned(),
            },
        }),
        attachment_objects: Vec::new(),
        spec: compiled.spec,
        workspace: WorkspaceBinding {
            workspace_id,
            path: workspace.path().to_string_lossy().into_owned(),
        },
        prior_events: Vec::new(),
        extensions: Vec::new(),
        display_input: None,
        source: None,
    };
    let envelope_path = workspace.path().join("envelope.json");
    tokio::fs::write(&envelope_path, serde_json::to_vec(&envelope).unwrap())
        .await
        .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ternilo-worker"));
    command
        .arg("execute")
        .arg("--policy")
        .arg(&policy_path)
        .arg("--envelope")
        .arg(&envelope_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    command.arg("--owned-process-session");
    let mut child = command.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    if authorize {
        input.write_all(b"ternilo-execute-v1\n").await.unwrap();
        input.flush().await.unwrap();
    }
    let output = BufReader::new(child.stdout.take().unwrap()).lines();
    (workspace, child, input, output)
}

pub(super) async fn next_model_request_with_inputs(
    output: &mut ChildOutput,
) -> (Value, Vec<Value>) {
    let mut inputs = Vec::new();
    loop {
        let line = tokio::time::timeout(Duration::from_secs(5), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("child protocol closed");
        let frame: Value = serde_json::from_str(&line).unwrap();
        if frame["type"] == "event" && frame["event"]["type"] == "user_message" {
            inputs.push(frame["event"].clone());
        }
        if frame["type"] == "model_request" {
            return (frame, inputs);
        }
    }
}

pub(super) async fn next_frame(output: &mut ChildOutput, expected_type: &str) -> Value {
    loop {
        let line = tokio::time::timeout(Duration::from_secs(5), output.next_line())
            .await
            .unwrap_or_else(|_| panic!("child protocol timed out waiting for {expected_type}"))
            .unwrap()
            .expect("child protocol closed");
        let frame: Value = serde_json::from_str(&line).unwrap();
        if frame["type"] == expected_type {
            return frame;
        }
        assert_ne!(frame["type"], "error", "child returned an error: {frame}");
    }
}

pub(super) async fn next_frame_answering_questions(
    input: &mut ChildStdin,
    output: &mut ChildOutput,
    expected_type: &str,
) -> Value {
    loop {
        let line = tokio::time::timeout(Duration::from_secs(5), output.next_line())
            .await
            .unwrap_or_else(|_| panic!("child protocol timed out waiting for {expected_type}"))
            .unwrap()
            .expect("child protocol closed");
        let frame: Value = serde_json::from_str(&line).unwrap();
        if frame["type"] == expected_type {
            return frame;
        }
        if frame["type"] == "question" {
            let label = frame["question"]["options"]
                .as_array()
                .and_then(|options| options.first())
                .and_then(|option| option["label"].as_str())
                .expect("tool approval question has an option");
            write_frame(
                input,
                json!({
                    "type": "question_answer",
                    "answer": {
                        "question_id": frame["question"]["id"],
                        "selected": [label],
                    },
                }),
            )
            .await;
            continue;
        }
        assert_ne!(frame["type"], "error", "child returned an error: {frame}");
    }
}

pub(super) async fn next_non_error_frame(output: &mut ChildOutput) -> Value {
    let line = tokio::time::timeout(Duration::from_secs(5), output.next_line())
        .await
        .expect("child protocol timed out")
        .unwrap()
        .expect("child protocol closed");
    let frame: Value = serde_json::from_str(&line).unwrap();
    assert_ne!(frame["type"], "error", "child returned an error: {frame}");
    frame
}

pub(super) async fn write_model_response(input: &mut ChildStdin, request_id: u64, content: &str) {
    write_frame(
        input,
        json!({
            "type": "model_complete",
            "request_id": request_id,
            "response": ModelResponse {
                provider: "controlled".to_owned(),
                model: "replace-with-provider-model-id".to_owned(),
                content: content.to_owned(),
                reasoning_content: None,
                provider_state: None,
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: ModelFinishReason::Stop,
                provider_request_id: Some(format!("controlled-{request_id}")),
                attempts: 1,
                request_digest: None,
                replayed: false,
            },
        }),
    )
    .await;
}

pub(super) async fn write_model_tool_response(
    input: &mut ChildStdin,
    request_id: u64,
    call_id: &str,
    name: &str,
    arguments: Value,
) {
    write_frame(
        input,
        json!({
            "type": "model_complete",
            "request_id": request_id,
            "response": ModelResponse {
                provider: "controlled".to_owned(),
                model: "replace-with-provider-model-id".to_owned(),
                content: String::new(),
                reasoning_content: None,
                provider_state: None,
                tool_calls: vec![ternilo_protocol::ToolCall {
                    id: call_id.to_owned(),
                    name: name.to_owned(),
                    arguments,
                    presentation: None,
                }],
                usage: None,
                finish_reason: ModelFinishReason::ToolCalls,
                provider_request_id: Some(format!("controlled-{request_id}")),
                attempts: 1,
                request_digest: None,
                replayed: false,
            },
        }),
    )
    .await;
}

pub(super) async fn assert_no_subagent_completion(output: &mut ChildOutput) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
    loop {
        let Ok(line) = tokio::time::timeout_at(deadline, output.next_line()).await else {
            return;
        };
        let frame: Value =
            serde_json::from_str(&line.unwrap().expect("child closed before admission")).unwrap();
        assert_ne!(
            frame["type"], "model_request",
            "background spawn must await durable admission"
        );
        assert_ne!(
            frame["event"]["type"], "tool_call_finished",
            "spawn cannot finish before admission"
        );
        assert_ne!(
            frame["type"], "host_request",
            "completion waiting begins only after admission"
        );
        assert_ne!(frame["type"], "error", "child returned an error: {frame}");
    }
}

pub(super) async fn write_host_admission(input: &mut ChildStdin, request: &Value) -> Value {
    let ticket = json!({
        "session_id": request["request"]["session_id"],
        "run_id": format!("accepted-{}", request["request"]["run_id"].as_str().unwrap()),
    });
    write_host_value(input, request, ticket.clone()).await;
    ticket
}

pub(super) async fn complete_scheduled_subagent(
    input: &mut ChildStdin,
    output: &mut ChildOutput,
    ticket: &Value,
    answer: &str,
) {
    let mut waiting = None;
    let mut park_revision = None;
    let parked_token = 701;
    // Admission and result waiting are independent tasks and may reach the pipe in either order.
    while waiting.is_none() || park_revision.is_none() {
        let request = next_admission_request(output).await;
        match request["request"]["operation"].as_str() {
            Some("wait_subagent") => {
                assert!(waiting.is_none(), "the accepted run is awaited once");
                assert_eq!(request["request"]["session_id"], ticket["session_id"]);
                assert_eq!(request["request"]["run_id"], ticket["run_id"]);
                waiting = Some(request);
            }
            Some("park_activity") => {
                assert!(park_revision.is_none(), "the foreground branch parks once");
                assert_eq!(request["request"]["dependencies"], json!([ticket]));
                let revision = request["request"]["activity_revision"].as_u64().unwrap();
                assert!(revision > 0, "activity revisions start above zero");
                park_revision = Some(revision);
                write_host_value(input, &request, json!(parked_token)).await;
            }
            _ => panic!("unexpected request while parking an accepted child: {request}"),
        }
    }
    write_host_run_outcome(input, &waiting.unwrap(), answer).await;
    let resume = next_admission_request(output).await;
    assert_eq!(resume["request"]["operation"], "resume_activity");
    assert_eq!(resume["request"]["parked_revision"], parked_token);
    assert!(
        resume["request"]["activity_revision"].as_u64().unwrap() > park_revision.unwrap(),
        "readmission uses a newer revision than the acknowledged park"
    );
    assert_no_subagent_completion(output).await;
    write_host_value(
        input,
        &resume,
        serde_json::to_value(ternilo_cloud::RunAdmission::Ready { admission_epoch: 2 }).unwrap(),
    )
    .await;
}

pub(super) async fn next_admission_request(output: &mut ChildOutput) -> Value {
    loop {
        let frame = next_non_error_frame(output).await;
        if frame["type"] == "host_request" {
            return frame;
        }
        assert_eq!(
            frame["type"], "event",
            "foreground work must remain blocked until readmission: {frame}"
        );
        assert_ne!(
            frame["event"]["type"], "tool_call_finished",
            "the waiting tool must not finish before readmission"
        );
    }
}

pub(super) async fn write_host_run_outcome(input: &mut ChildStdin, request: &Value, answer: &str) {
    write_frame(
        input,
        json!({
            "type": "host_reply",
            "request_id": request["request_id"],
            "outcome": {
                "status": "ok",
                "value": {
                    "answer": answer,
                    "steps": 1,
                    "tool_calls": 0,
                    "events": [],
                },
            },
        }),
    )
    .await;
}

pub(super) async fn write_host_value(input: &mut ChildStdin, request: &Value, value: Value) {
    write_frame(
        input,
        json!({
            "type": "host_reply",
            "request_id": request["request_id"],
            "outcome": {
                "status": "ok",
                "value": value,
            },
        }),
    )
    .await;
}

pub(super) async fn write_frame(input: &mut ChildStdin, value: Value) {
    let mut encoded = serde_json::to_vec(&value).unwrap();
    encoded.push(b'\n');
    input.write_all(&encoded).await.unwrap();
    input.flush().await.unwrap();
}

async fn install_workspace_skill(workspace: &std::path::Path) {
    let skill_dir = workspace.join(".agents/skills/release-check");
    tokio::fs::create_dir_all(&skill_dir).await.unwrap();
    tokio::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: release-check\ndescription: Check a release\nuser-invocable: true\n---\n# Release check\nVerify the controlled provider payload.",
    )
    .await
    .unwrap();
}
