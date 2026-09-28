use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify the complete Agent Team tool surface through one real child and parent exchange."
)]
async fn controlled_child_exposes_all_agent_team_tools_through_the_parent_host() {
    let (_workspace, mut child, mut input, mut output) =
        spawn_controlled_child("controlled-team-run", "inspect the team").await;
    let first = next_frame(&mut output, "model_request").await;
    let tools = first["request"]["tools"].as_array().unwrap();
    for name in [
        "team_task_list",
        "team_task_create",
        "team_task_update",
        "team_task_delete",
        "team_mailbox",
        "team_message_send",
        "team_message_read",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == name),
            "missing {name}"
        );
    }
    write_model_tool_response(
        &mut input,
        first["request_id"].as_u64().unwrap(),
        "team-list-1",
        "team_task_list",
        json!({}),
    )
    .await;
    let host = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(host["request"]["operation"], "agent_team_snapshot");
    write_frame(
        &mut input,
        json!({
            "type": "host_reply",
            "request_id": host["request_id"],
            "outcome": {
                "status": "ok",
                "value": {
                    "team_id": "team-controlled",
                    "current_member_id": "member-lead",
                    "members": team_members(),
                    "tasks": [],
                    "messages": []
                }
            }
        }),
    )
    .await;
    let second = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    assert!(
        second["request"]["messages"]
            .to_string()
            .contains("team-controlled")
    );
    write_model_tool_response(
        &mut input,
        second["request_id"].as_u64().unwrap(),
        "team-create-1",
        "team_task_create",
        json!({
            "subject": "Inspect persistence",
            "owner": "member-child"
        }),
    )
    .await;
    let create = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(create["request"]["operation"], "agent_team_task_create");
    assert_eq!(
        create["request"]["request"]["subject"],
        "Inspect persistence"
    );
    assert_eq!(create["request"]["request"]["owner"], "member-child");
    write_host_value(
        &mut input,
        &create,
        team_task("task-controlled", "Inspect persistence", "pending", 1),
    )
    .await;

    let third = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    assert!(
        third["request"]["messages"]
            .to_string()
            .contains("task-controlled")
    );
    write_model_tool_response(
        &mut input,
        third["request_id"].as_u64().unwrap(),
        "team-update-1",
        "team_task_update",
        json!({
            "task_id": "task-controlled",
            "expected_revision": 1,
            "subject": "Inspect persistence",
            "description": "updated by the lead",
            "status": "in_progress",
            "dependencies": [],
            "owner": "member-child"
        }),
    )
    .await;
    let update = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(update["request"]["operation"], "agent_team_task_replace");
    assert_eq!(update["request"]["task_id"], "task-controlled");
    assert_eq!(update["request"]["request"]["expected_revision"], 1);
    write_host_value(
        &mut input,
        &update,
        team_task("task-controlled", "Inspect persistence", "in_progress", 2),
    )
    .await;

    let fourth = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    write_model_tool_response(
        &mut input,
        fourth["request_id"].as_u64().unwrap(),
        "team-mailbox-root-1",
        "team_mailbox",
        json!({}),
    )
    .await;
    let root_mailbox =
        next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(root_mailbox["request"]["operation"], "agent_team_snapshot");
    write_host_value(
        &mut input,
        &root_mailbox,
        json!({
            "team_id": "team-controlled",
            "current_member_id": "member-lead",
            "members": team_members(),
            "tasks": [team_task("task-controlled", "Inspect persistence", "in_progress", 2)],
            "messages": []
        }),
    )
    .await;

    let fifth = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    write_model_tool_response(
        &mut input,
        fifth["request_id"].as_u64().unwrap(),
        "team-send-1",
        "team_message_send",
        json!({
            "to": "member-child",
            "content": "Check the durable child"
        }),
    )
    .await;
    let send = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(send["request"]["operation"], "agent_team_message_send");
    assert_eq!(send["request"]["request"]["to"], "member-child");
    let unread_message = json!({
        "id": "message-controlled",
        "from": "member-lead",
        "to": "member-child",
        "content": "Check the durable child",
        "created_at_ms": 100
    });
    write_host_value(&mut input, &send, unread_message.clone()).await;

    let sixth = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    write_model_tool_response(
        &mut input,
        sixth["request_id"].as_u64().unwrap(),
        "team-delete-1",
        "team_task_delete",
        json!({
            "task_id": "task-controlled",
            "expected_revision": 2
        }),
    )
    .await;
    let delete = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(delete["request"]["operation"], "agent_team_task_delete");
    assert_eq!(delete["request"]["task_id"], "task-controlled");
    assert_eq!(delete["request"]["expected_revision"], 2);
    write_host_value(&mut input, &delete, Value::Null).await;

    let seventh = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    write_model_tool_response(
        &mut input,
        seventh["request_id"].as_u64().unwrap(),
        "team-mailbox-child-1",
        "team_mailbox",
        json!({}),
    )
    .await;
    let child_mailbox =
        next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(child_mailbox["request"]["operation"], "agent_team_snapshot");
    write_host_value(
        &mut input,
        &child_mailbox,
        json!({
            "team_id": "team-controlled",
            "current_member_id": "member-child",
            "members": team_members(),
            "tasks": [],
            "messages": [unread_message]
        }),
    )
    .await;

    let eighth = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    assert!(
        eighth["request"]["messages"]
            .to_string()
            .contains("message-controlled")
    );
    write_model_tool_response(
        &mut input,
        eighth["request_id"].as_u64().unwrap(),
        "team-read-1",
        "team_message_read",
        json!({ "message_id": "message-controlled" }),
    )
    .await;
    let read = next_frame_answering_questions(&mut input, &mut output, "host_request").await;
    assert_eq!(read["request"]["operation"], "agent_team_message_read");
    assert_eq!(read["request"]["message_id"], "message-controlled");
    write_host_value(
        &mut input,
        &read,
        json!({
            "id": "message-controlled",
            "from": "member-lead",
            "to": "member-child",
            "content": "Check the durable child",
            "created_at_ms": 100,
            "read_at_ms": 101
        }),
    )
    .await;

    let ninth = next_frame_answering_questions(&mut input, &mut output, "model_request").await;
    assert!(
        ninth["request"]["messages"]
            .to_string()
            .contains("read_at_ms")
    );
    write_model_response(
        &mut input,
        ninth["request_id"].as_u64().unwrap(),
        "team operations completed",
    )
    .await;
    loop {
        let frame = next_non_error_frame(&mut output).await;
        match frame["type"].as_str() {
            Some("model_request") => {
                write_model_response(
                    &mut input,
                    frame["request_id"].as_u64().unwrap(),
                    "Controlled Team title",
                )
                .await;
            }
            Some("outcome") => {
                assert_eq!(frame["outcome"]["answer"], "team operations completed");
                break;
            }
            _ => {}
        }
    }
    drop(input);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("Agent Team child did not exit")
            .unwrap()
            .success()
    );
}

fn team_members() -> Value {
    json!([
        {
            "id": "member-lead",
            "label": "Lead",
            "role": "lead"
        },
        {
            "id": "member-child",
            "parent_id": "member-lead",
            "subagent_id": "researcher",
            "label": "Researcher",
            "provider": "in-process",
            "role": "subagent"
        }
    ])
}

fn team_task(id: &str, subject: &str, status: &str, revision: u64) -> Value {
    json!({
        "id": id,
        "subject": subject,
        "description": "updated by the lead",
        "status": status,
        "dependencies": [],
        "owner": "member-child",
        "revision": revision,
        "created_at_ms": 90,
        "updated_at_ms": 90 + revision
    })
}
