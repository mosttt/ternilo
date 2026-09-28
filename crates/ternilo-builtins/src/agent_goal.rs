use ternilo_kernel::CommandResolution;
use ternilo_protocol::{GoalStatus, MessageRole, ModelMessage, SessionEvent, SessionEventKind};

pub(crate) fn starts_goal_execution(input: &str, command: Option<&CommandResolution>) -> bool {
    command.is_some_and(|command| {
        command.command_name == "goal"
            && input.split_whitespace().nth(1) != Some("edit")
            && command.result.as_ref().is_ok_and(|resolved| {
                resolved.tool_name == "update_goal" && resolved.arguments["status"] == "active"
            })
    })
}

pub(crate) fn current_goal(events: &[SessionEvent]) -> Option<(String, GoalStatus)> {
    ternilo_protocol::conversation_events(events)
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            SessionEventKind::GoalUpdated { objective, status } => {
                Some((objective.clone(), *status))
            }
            _ => None,
        })
}

pub(crate) fn round_prompt(objective: &str, round: u32, max_rounds: u32) -> String {
    format!(
        "<goal_round>\nObjective: {}\nRound: {round}\nRound limit: {}\n\nContinue executing this user-requested goal in the same session. Use the actual workspace, tool results and current goal state as evidence. Make concrete progress, not just a promise to continue. When the entire objective is verified, call update_goal with this objective and status complete, then give the user the result. If a genuine blocker prevents further progress, call update_goal with status blocked and explain what is needed. Otherwise leave the goal active; execution will continue automatically. Follow new human instructions and do not claim completion merely because this round is ending.\n</goal_round>",
        serde_json::to_string(objective).expect("serialize goal objective"),
        if max_rounds == 0 {
            "unlimited".to_owned()
        } else {
            max_rounds.to_string()
        },
    )
}

pub(crate) fn model_message(kind: &SessionEventKind) -> Option<ModelMessage> {
    let content = match kind {
        SessionEventKind::GoalUpdated { objective, status } => format!(
            "<session_goal_state>\n{}\nThis is saved runtime state, not a new human request or authorization to restart stopped work. Follow the current human request and the current authorized goal round.\n</session_goal_state>",
            serde_json::json!({ "objective": objective, "status": status }),
        ),
        SessionEventKind::GoalRoundStarted {
            objective,
            round,
            max_rounds,
        } => round_prompt(objective, *round, *max_rounds),
        _ => return None,
    };
    Some(ModelMessage {
        role: MessageRole::User,
        content,
        reasoning_content: None,
        provider_state: None,
        attachments: Vec::new(),
        tool_call_id: None,
        tool_calls: Vec::new(),
    })
}
