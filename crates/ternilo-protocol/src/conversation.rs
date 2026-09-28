use std::{borrow::Cow, collections::BTreeSet};

use crate::{HarnessError, SessionEvent, SessionEventKind, UserMessageSource};

impl SessionEvent {
    #[must_use]
    pub const fn regeneration_target(&self) -> Option<u64> {
        match &self.kind {
            SessionEventKind::UserMessage {
                source:
                    Some(UserMessageSource::Submission {
                        regenerate_from, ..
                    }),
                ..
            } => *regenerate_from,
            _ => None,
        }
    }
}

#[must_use]
pub fn conversation_events(events: &[SessionEvent]) -> Cow<'_, [SessionEvent]> {
    if !events
        .iter()
        .any(|event| event.regeneration_target().is_some())
    {
        return Cow::Borrowed(events);
    }
    let mut active: Vec<&SessionEvent> = Vec::new();
    let mut discarded = BTreeSet::new();
    let mut changed = false;
    for event in events {
        if let Some(target) = event.regeneration_target()
            && let Some(original) = active.iter().find(|entry| entry.seq == target)
        {
            let target_run = &original.run_id;
            let boundary = if active.iter().any(|entry| {
                entry.seq < target
                    && &entry.run_id == target_run
                    && matches!(entry.kind, SessionEventKind::UserMessage { .. })
            }) {
                active.iter().position(|entry| entry.seq == target).unwrap()
            } else {
                active
                    .iter()
                    .position(|entry| &entry.run_id == target_run)
                    .unwrap()
            };
            let current_start = active
                .iter()
                .position(|entry| entry.run_id == event.run_id)
                .unwrap_or(active.len());
            let current = active.split_off(current_start);
            discarded.extend(active.drain(boundary..).map(|entry| entry.run_id.clone()));
            active.extend(current);
            changed = true;
        }
        if !discarded.contains(&event.run_id) {
            active.push(event);
        }
    }
    if changed {
        Cow::Owned(active.into_iter().cloned().collect())
    } else {
        Cow::Borrowed(events)
    }
}

pub fn validate_regeneration(events: &[SessionEvent], target_seq: u64) -> Result<(), HarnessError> {
    let active = conversation_events(events);
    active
        .iter()
        .find(|event| event.seq == target_seq)
        .filter(|event| matches!(event.kind, SessionEventKind::UserMessage { .. }))
        .ok_or_else(|| {
            HarnessError::conflict("the message is no longer in the current conversation")
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunId, SubmissionDelivery, SubmissionId};

    fn event(seq: u64, run: &str, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            seq,
            run_id: RunId::new(run),
            occurred_at_ms: seq,
            kind,
        }
    }

    fn user(seq: u64, run: &str, target: Option<u64>) -> SessionEvent {
        event(
            seq,
            run,
            SessionEventKind::UserMessage {
                content: run.to_owned(),
                display_content: None,
                provenance: None,
                source: Some(UserMessageSource::Submission {
                    submission_id: SubmissionId::new(format!("input-{run}")),
                    created_at_ms: seq,
                    delivery: SubmissionDelivery::Queue,
                    skill_name: None,
                    regenerate_from: target,
                }),
                references: Vec::new(),
                attachments: Vec::new(),
            },
        )
    }

    #[test]
    fn regeneration_replaces_the_selected_turn_and_its_followups_without_rewriting_facts() {
        let mut events = vec![
            event(0, "prior", SessionEventKind::TurnStarted),
            user(1, "prior", None),
            event(2, "original", SessionEventKind::TurnStarted),
            user(3, "original", None),
            event(4, "followup", SessionEventKind::TurnStarted),
            user(5, "followup", None),
            event(6, "replacement", SessionEventKind::TurnStarted),
            user(7, "replacement", Some(3)),
            event(
                8,
                "original",
                SessionEventKind::SessionTitleGenerated {
                    title: "late result".to_owned(),
                },
            ),
        ];
        let unchanged = events.clone();
        let active = conversation_events(&events);
        assert_eq!(
            active.iter().map(|event| event.seq).collect::<Vec<_>>(),
            [0, 1, 6, 7]
        );
        assert_eq!(conversation_events(&active).as_ref(), active.as_ref());
        assert_eq!(events, unchanged);
        assert!(validate_regeneration(&events, 3).is_err());
        assert!(validate_regeneration(&events, 7).is_ok());
        events.extend([
            event(9, "retried", SessionEventKind::TurnStarted),
            user(10, "retried", Some(7)),
        ]);
        assert_eq!(
            conversation_events(&events)
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [0, 1, 9, 10]
        );
    }

    #[test]
    fn regeneration_accepts_each_input_and_rejects_missing_or_non_input_targets() {
        let events = vec![
            event(0, "turn", SessionEventKind::TurnStarted),
            user(1, "turn", None),
            user(2, "turn", None),
        ];
        assert!(validate_regeneration(&events, 0).is_err());
        assert!(validate_regeneration(&events, 100).is_err());
        assert!(validate_regeneration(&events, 2).is_ok());
        assert!(validate_regeneration(&events, 1).is_ok());
    }

    #[test]
    fn replacing_a_batch_message_keeps_its_prefix_and_discards_later_inputs_and_events() {
        let events = vec![
            event(0, "batch", SessionEventKind::TurnStarted),
            user(1, "batch", None),
            user(2, "batch", None),
            user(3, "batch", None),
            event(4, "batch", SessionEventKind::TurnCancelled),
            event(5, "replacement", SessionEventKind::TurnStarted),
            user(6, "replacement", Some(2)),
            event(7, "batch", SessionEventKind::TurnCancelled),
        ];
        let active = conversation_events(&events);
        assert_eq!(
            active.iter().map(|event| event.seq).collect::<Vec<_>>(),
            [0, 1, 5, 6]
        );
        assert_eq!(conversation_events(&active).as_ref(), active.as_ref());
        assert!(validate_regeneration(&events, 1).is_ok());
        assert!(validate_regeneration(&events, 3).is_err());
    }
}
