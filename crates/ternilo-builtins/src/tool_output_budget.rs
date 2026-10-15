use ternilo_protocol::{MessageRole, ModelMessage};

/// Bound model-facing tool content while retaining every call/result pair.
/// The durable event log and retained attachments are not changed.
pub(crate) fn apply(messages: &mut [ModelMessage], budget: usize) {
    if budget == 0 {
        return;
    }
    let mut remaining_count = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .count();
    if remaining_count == 0 {
        return;
    }
    let reserve = (budget / remaining_count).min(128);
    let mut remaining = budget;
    for message in messages
        .iter_mut()
        .rev()
        .filter(|message| message.role == MessageRole::Tool)
    {
        remaining_count -= 1;
        let allowed = remaining.saturating_sub(remaining_count.saturating_mul(reserve));
        let length = message.content.chars().count();
        if length > allowed {
            message.content = preview(&message.content, allowed);
        }
        remaining = remaining.saturating_sub(message.content.chars().count());
    }
}

pub(super) fn preview(content: &str, limit: usize) -> String {
    const MARKER: &str = "\n[Tool output shortened to fit the context budget; full output remains in session history.]\n";
    if content.chars().count() <= limit {
        return content.to_owned();
    }
    let marker_size = MARKER.chars().count();
    if limit <= marker_size {
        return MARKER.chars().take(limit).collect();
    }
    let available = limit - marker_size;
    let tail = available / 4;
    let mut result: String = content.chars().take(available - tail).collect();
    result.push_str(MARKER);
    let ending: String = content.chars().rev().take(tail).collect();
    result.extend(ending.chars().rev());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_history_budget_keeps_tool_pairs_and_prefers_recent_results_without_changing_user_text() {
        let message = |role, content: String, id| ModelMessage {
            role,
            content,
            reasoning_content: None,
            provider_state: None,
            attachments: vec![],
            tool_calls: vec![],
            tool_call_id: id,
        };
        let mut messages = vec![
            message(MessageRole::User, "Keep the actual constraint".into(), None),
            message(
                MessageRole::Tool,
                "旧내용".repeat(1_000),
                Some("old".into()),
            ),
            message(
                MessageRole::Tool,
                "new结果".repeat(1_000),
                Some("new".into()),
            ),
        ];
        apply(&mut messages, 1024);
        assert_eq!(messages[0].content, "Keep the actual constraint");
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("old"));
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("new"));
        assert!(messages[2].content.contains("new结果"));
        let lengths: Vec<_> = messages[1..]
            .iter()
            .map(|message| message.content.chars().count())
            .collect();
        assert_eq!(lengths.iter().sum::<usize>(), 1024);
        assert!(lengths[1] > lengths[0]);
        assert!(messages[1].content.contains("session history"));
        let original = messages.clone();
        apply(&mut messages, 0);
        assert_eq!(original, messages);
    }
}
