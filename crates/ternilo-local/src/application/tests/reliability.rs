use super::*;
use serde_json::{Value, json};

fn code_result(outcome: &ternilo_protocol::RunOutcome) -> Value {
    let output = outcome
        .events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::ToolCallFinished { output, .. } => Some(output),
            _ => None,
        })
        .expect("code result");
    assert!(!output.is_error, "{}", output.content);
    serde_json::from_str::<Value>(&output.content).unwrap()["result"].clone()
}

#[tokio::test]
async fn reviewer_checks_are_named_read_only_and_survive_restart() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("workspace");
    tokio::fs::create_dir_all(folder.join("tests"))
        .await
        .unwrap();
    tokio::fs::write(folder.join("tests/test_sample.py"), "import unittest\nclass Checks(unittest.TestCase):\n def test_value(self): self.assertEqual(2 + 2, 4)\n").await.unwrap();
    let data = root.path().join("data");
    let app = open_test_application(data.clone()).await;
    let workspace = app.add_workspace(folder.to_str().unwrap()).await.unwrap();
    let parent = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    app.update_permissions(
        parent.identity.session_id.as_str(),
        PermissionPreset::FullAccess,
    )
    .await
    .unwrap();
    // Test retention after output, allowing the isolated login shell time to start.
    app.update_profile_plugins(parent.identity.session_id.as_str(), vec![PluginEntry {
        id: "shell-tool".into(), kind: ternilo_builtins::SHELL_TOOL_KIND.into(), enabled: true,
        config: json!({"checks":[
            {"name":"unit", "command":"python3 -m unittest discover -s tests", "timeout_ms":10000},
            {"name":"blocked", "command":"touch forbidden.txt"},
            {"name":"temporary", "command":"printf '%s' \"$TMPDIR\"; printf cache > \"$TMPDIR/check\""},
            {"name":"timeout", "command":"printf before; printf diagnostic >&2; sleep 30", "timeout_ms":2000}
        ]}),
    }]).await.unwrap();
    let host = app.subagent_session_host();
    let child = host
        .create(
            parent.identity.clone(),
            ternilo_kernel::SubagentSessionRequest {
                role: Some(ternilo_protocol::AgentRole::Reviewer),
                subagent_id: ternilo_protocol::SubagentId::new("review"),
                provider: "in-process".into(),
                label: "Review".into(),
                task: "verify tests".into(),
                transcript_kind: ternilo_protocol::SubagentTranscriptKind::Conversation,
            },
        )
        .await
        .unwrap()
        .unwrap();
    let outcome = app.run_turn(child.session_id.as_str(), None, r#"/code #{unit:try_tool("run_check",#{name:"unit"}), blocked:try_tool("run_check",#{name:"blocked"}), temporary:try_tool("run_check",#{name:"temporary"}), timeout:try_tool("run_check",#{name:"timeout"}), injected:try_tool("run_check",#{name:"unit", command:"touch injected.txt"}), unknown:try_tool("run_check",#{name:"unknown"}), shell:try_tool("shell",#{command:"touch shell.txt"}), write:try_tool("write_file",#{path:"write.txt",content:"blocked"})}"#.into()).await.unwrap();
    let value = code_result(&outcome);
    assert_eq!(value["unit"]["ok"], true, "{value}");
    assert!(
        value["unit"]["result"]["stderr"]
            .as_str()
            .unwrap()
            .contains("OK")
    );
    for name in [
        "blocked", "timeout", "injected", "unknown", "shell", "write",
    ] {
        assert_eq!(value[name]["ok"], false, "{name}: {value}");
    }
    assert_eq!(value["timeout"]["result"]["timed_out"], true);
    assert_eq!(value["timeout"]["result"]["stdout"], "before");
    assert!(
        value["timeout"]["result"]["stderr"]
            .as_str()
            .unwrap()
            .contains("diagnostic")
    );
    let temp = value["temporary"]["result"]["stdout"].as_str().unwrap();
    assert!(
        !std::path::Path::new(temp).exists(),
        "temporary files must be reaped with the process"
    );
    for name in ["forbidden.txt", "injected.txt", "shell.txt", "write.txt"] {
        assert!(!folder.join(name).exists());
    }
    // The new tool cannot bypass the sandbox even for a full-access lead.
    let outcome = app
        .run_turn(
            parent.identity.session_id.as_str(),
            None,
            r#"/code try_tool("run_check",#{name:"blocked"})"#.into(),
        )
        .await
        .unwrap();
    assert_eq!(code_result(&outcome)["ok"], false);
    assert!(!folder.join("forbidden.txt").exists());
    drop(host);
    app.shutdown().await.unwrap();
    drop(app);
    let app = open_test_application(data).await;
    let outcome = app
        .run_turn(
            child.session_id.as_str(),
            None,
            r#"/code try_tool("run_check",#{name:"unit"})"#.into(),
        )
        .await
        .unwrap();
    assert_eq!(code_result(&outcome)["ok"], true);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn structured_git_failure_and_code_map_details_keep_original_evidence() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("workspace");
    tokio::fs::create_dir(&folder).await.unwrap();
    tokio::fs::write(
        folder.join("sample.py"),
        "def helper():\n return 42\ndef main():\n print(len(str(helper())))\nmain()\n",
    )
    .await
    .unwrap();
    let app = open_test_application(root.path().join("data")).await;
    let workspace = app.add_workspace(folder.to_str().unwrap()).await.unwrap();
    let session = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let outcome = app.run_turn(session.identity.session_id.as_str(), None, r#"/code let failed = try_tool("git_inspect",#{action:"status"}); let summary = tools::code_map(#{paths:["sample.py"]}); let full = tools::code_map(#{paths:["sample.py"], detail:"full"}); #{failed:failed, summary:summary, full:full}"#.into()).await.unwrap();
    let value = code_result(&outcome);
    assert_eq!(value["failed"]["ok"], false);
    assert_eq!(value["failed"]["result"]["operations"][0]["exit_code"], 128);
    assert!(
        value["failed"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a git repository")
    );
    assert!(
        !value["failed"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("{\"operations")
    );
    assert_eq!(
        value["summary"]["files"][0]["symbols"],
        value["full"]["files"][0]["symbols"]
    );
    let compact = value["summary"]["files"][0]["calls"].as_array().unwrap();
    let full = value["full"]["files"][0]["calls"].as_array().unwrap();
    assert_eq!(compact.len(), 2);
    assert_eq!(full.len(), 5);
    assert_eq!(value["summary"]["files"][0]["omitted_calls"], 3);
    assert!(compact.iter().any(|call| call["target"] == "helper"));
    // Existing throwing calls now give a useful summary; the dispatch retains the full result.
    let outcome = app
        .run_turn(
            session.identity.session_id.as_str(),
            None,
            r#"/code tools::git_inspect(#{action:"status"})"#.into(),
        )
        .await
        .unwrap();
    assert!(outcome.answer.contains("not a git repository"));
    assert!(!outcome.answer.contains("{\"operations"));
    app.shutdown().await.unwrap();
}
