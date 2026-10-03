use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "verify two real ACP process trees, account revocation and workspace release"
)]
async fn revocation_waits_for_external_acp_descendants_and_preserves_the_other_account() {
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("acp_writer.py");
    tokio::fs::write(&fixture, r#"import json
import subprocess
import sys

writer = "import signal,time\nsignal.signal(signal.SIGTERM, signal.SIG_IGN)\nwhile True:\n with open('heartbeat', 'ab') as f: f.write(b'x')\n time.sleep(0.01)\n"
for line in sys.stdin:
    frame = json.loads(line)
    method = frame.get('method')
    if method == 'initialize':
        result = {'protocolVersion': 1}
    elif method == 'session/new':
        result = {'sessionId': 'fixture-session'}
    elif method == 'session/prompt':
        subprocess.Popen([sys.executable, '-c', writer], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        continue
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': frame['id'], 'result': result}), flush=True)
"#).await.unwrap();
    let app = open(root.path().join("data"), true).await;
    app.synchronize_account_authorizations(&snapshot(1, true, false))
        .await
        .unwrap();
    let mut directories = Vec::new();
    for name in ["alice", "bob"] {
        let directory = root.path().join(name);
        tokio::fs::create_dir(&directory).await.unwrap();
        let workspace = app
            .add_workspace(directory.to_str().unwrap())
            .await
            .unwrap();
        app.create_session(workspace.workspace_id, Some(name.into()), None)
            .await
            .unwrap();
        app.update_permissions(name, ternilo_protocol::PermissionPreset::FullAccess)
            .await
            .unwrap();
        app.update_profile_plugins(name, vec![PluginEntry {
            id: "fixture-acp".into(), kind: ternilo_builtins::ACP_SUBAGENT_KIND.into(), enabled: true,
            config: serde_json::json!({ "provider_name": "fixture", "command": "python3", "args": [fixture], "permission": "reject", "shutdown_grace_ms": 50 }),
        }]).await.unwrap();
        let input = account_input(&format!("{name}-acp"), name);
        app.record_account_input_authorization(&input, &proof(1))
            .await
            .unwrap();
        let running = {
            let app = Arc::clone(&app);
            tokio::spawn(async move {
                app.run_session_input_with_provenance(
                    name,
                    request("/agent-bg-on fixture write until cancelled", name),
                    input,
                )
                .await
            })
        };
        let question = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(question) = app.pending_questions(Some(name)).await.into_iter().next() {
                    break question;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            question.question.tool_approval.as_ref().unwrap().tool_name,
            "spawn_agent"
        );
        app.answer_question(ternilo_protocol::UserAnswer {
            question_id: question.question.id,
            selected: vec!["Allow once".into()],
            custom: None,
        })
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        directories.push(directory);
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while directories
            .iter()
            .any(|path| !path.join("heartbeat").exists())
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let receipts = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        app.synchronize_account_authorizations(&snapshot(2, false, true)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        receipts[0].state,
        NodeCleanupState::Confirmed,
        "{receipts:?}"
    );
    let alice_bytes = tokio::fs::metadata(directories[0].join("heartbeat"))
        .await
        .unwrap()
        .len();
    let bob_bytes = tokio::fs::metadata(directories[1].join("heartbeat"))
        .await
        .unwrap()
        .len();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        alice_bytes,
        tokio::fs::metadata(directories[0].join("heartbeat"))
            .await
            .unwrap()
            .len()
    );
    assert!(
        bob_bytes
            < tokio::fs::metadata(directories[1].join("heartbeat"))
                .await
                .unwrap()
                .len()
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.run_turn("alice", None, "/shell printf released > successor".into()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(directories[0].join("successor"))
            .await
            .unwrap(),
        "released"
    );
    app.close().await.unwrap();
    assert!(app.execution_resources.owners().await.is_empty());
}
