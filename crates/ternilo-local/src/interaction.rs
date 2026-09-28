use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use serde::Serialize;
use ternilo_kernel::UserInteraction;
use ternilo_protocol::{HarnessError, SessionId, UserAnswer, UserQuestion};
use tokio::sync::{Mutex, broadcast, oneshot};

use crate::{
    LocalInvalidationCategory, LocalInvalidationNotification, notifications::publish_invalidation,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PendingQuestion {
    pub session_id: SessionId,
    pub question: UserQuestion,
}

struct Pending {
    session_id: SessionId,
    question: UserQuestion,
    sender: oneshot::Sender<UserAnswer>,
}

pub struct InteractionBroker {
    pending: Mutex<BTreeMap<String, Pending>>,
    invalidations: Option<broadcast::Sender<LocalInvalidationNotification>>,
}

impl Default for InteractionBroker {
    fn default() -> Self {
        Self {
            pending: Mutex::new(BTreeMap::new()),
            invalidations: None,
        }
    }
}

impl InteractionBroker {
    pub(crate) fn with_invalidations(
        invalidations: broadcast::Sender<LocalInvalidationNotification>,
    ) -> Self {
        Self {
            pending: Mutex::new(BTreeMap::new()),
            invalidations: Some(invalidations),
        }
    }

    #[must_use]
    pub fn for_session(self: &Arc<Self>, session_id: SessionId) -> Arc<dyn UserInteraction> {
        Arc::new(SessionInteraction {
            broker: Arc::clone(self),
            session_id,
        })
    }

    pub async fn pending(&self, session_id: Option<&str>) -> Vec<PendingQuestion> {
        self.pending
            .lock()
            .await
            .values()
            .filter(|pending| session_id.is_none_or(|id| pending.session_id.as_str() == id))
            .map(|pending| PendingQuestion {
                session_id: pending.session_id.clone(),
                question: pending.question.clone(),
            })
            .collect()
    }

    pub async fn answer(&self, answer: UserAnswer) -> Result<SessionId, HarnessError> {
        let mut pending_questions = self.pending.lock().await;
        let pending = pending_questions.get(&answer.question_id).ok_or_else(|| {
            HarnessError::invalid(format!(
                "unknown or already answered question {:?}",
                answer.question_id
            ))
        })?;
        if !pending.question.multi_select && answer.selected.len() > 1 {
            return Err(HarnessError::invalid(
                "single-select question accepts at most one selected option",
            ));
        }
        if !pending.question.multi_select
            && !answer.selected.is_empty()
            && answer
                .custom
                .as_deref()
                .is_some_and(|value| !value.is_empty())
        {
            return Err(HarnessError::invalid(
                "single-select question accepts either one option or custom text",
            ));
        }
        if answer.selected.iter().any(|selected| {
            !pending
                .question
                .options
                .iter()
                .any(|option| option.label == *selected)
        }) {
            return Err(HarnessError::invalid(
                "question answer selected an option that was not offered",
            ));
        }
        let pending = pending_questions
            .remove(&answer.question_id)
            .expect("pending question existed while broker lock was held");
        drop(pending_questions);
        let session_id = pending.session_id;
        pending
            .sender
            .send(answer)
            .map_err(|_| HarnessError::execution("question waiter stopped before the answer"))?;
        Ok(session_id)
    }

    pub async fn cancel_session(&self, session_id: &str) {
        self.pending
            .lock()
            .await
            .retain(|_, pending| pending.session_id.as_str() != session_id);
    }

    async fn ask(
        &self,
        session_id: SessionId,
        question: UserQuestion,
    ) -> Result<UserAnswer, HarnessError> {
        let (sender, receiver) = oneshot::channel();
        let mut pending = self.pending.lock().await;
        if pending.contains_key(&question.id) {
            return Err(HarnessError::invalid(format!(
                "duplicate pending question {:?}",
                question.id
            )));
        }
        pending.insert(
            question.id.clone(),
            Pending {
                session_id: session_id.clone(),
                question,
                sender,
            },
        );
        drop(pending);
        if let Some(invalidations) = &self.invalidations {
            publish_invalidation(
                invalidations,
                Some(session_id.as_str()),
                LocalInvalidationCategory::Questions,
                None,
            );
        }
        receiver
            .await
            .map_err(|_| HarnessError::execution("question channel closed without an answer"))
    }
}

struct SessionInteraction {
    broker: Arc<InteractionBroker>,
    session_id: SessionId,
}

impl UserInteraction for SessionInteraction {
    fn ask<'a>(
        &'a self,
        question: UserQuestion,
    ) -> Pin<Box<dyn Future<Output = Result<UserAnswer, HarnessError>> + Send + 'a>> {
        Box::pin(self.broker.ask(self.session_id.clone(), question))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::UserQuestionOption;

    fn question(multi_select: bool) -> UserQuestion {
        UserQuestion {
            id: "question-1".to_owned(),
            question: "What should change?".to_owned(),
            detail: Some("Choose every relevant surface.".to_owned()),
            header: Some("Scope".to_owned()),
            options: vec![
                UserQuestionOption {
                    label: "Tests".to_owned(),
                    description: Some("Update focused coverage.".to_owned()),
                },
                UserQuestionOption {
                    label: "Docs".to_owned(),
                    description: None,
                },
            ],
            multi_select,
            presentation: None,
            tool_approval: None,
        }
    }

    #[tokio::test]
    async fn structured_multi_select_answer_survives_the_broker() {
        let broker = Arc::new(InteractionBroker::default());
        let waiter = Arc::clone(&broker);
        let task = tokio::spawn(async move {
            waiter
                .ask(SessionId::new("session"), question(true))
                .await
                .unwrap()
        });
        while broker.pending(Some("session")).await.is_empty() {
            tokio::task::yield_now().await;
        }
        let answer = UserAnswer {
            question_id: "question-1".to_owned(),
            selected: vec!["Tests".to_owned(), "Docs".to_owned()],
            custom: Some("Release notes".to_owned()),
        };
        broker.answer(answer.clone()).await.unwrap();
        assert_eq!(task.await.unwrap(), answer.clone());
        assert!(broker.pending(Some("session")).await.is_empty());
        let duplicate = broker.answer(answer).await.unwrap_err();
        assert!(duplicate.to_string().contains("already answered"));
    }

    #[tokio::test]
    async fn invalid_single_select_answer_does_not_consume_the_question() {
        let broker = Arc::new(InteractionBroker::default());
        let waiter = Arc::clone(&broker);
        let task = tokio::spawn(async move {
            waiter
                .ask(SessionId::new("session"), question(false))
                .await
                .unwrap()
        });
        while broker.pending(Some("session")).await.is_empty() {
            tokio::task::yield_now().await;
        }
        let error = broker
            .answer(UserAnswer {
                question_id: "question-1".to_owned(),
                selected: vec!["Tests".to_owned(), "Docs".to_owned()],
                custom: None,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("at most one"));
        assert_eq!(broker.pending(Some("session")).await.len(), 1);
        broker
            .answer(UserAnswer {
                question_id: "question-1".to_owned(),
                selected: vec!["Tests".to_owned()],
                custom: None,
            })
            .await
            .unwrap();
        assert!(task.await.unwrap().chose("Tests"));
    }

    #[tokio::test]
    async fn cancelling_a_session_releases_its_waiter_without_touching_other_sessions() {
        let broker = Arc::new(InteractionBroker::default());
        let cancelled_waiter = Arc::clone(&broker);
        let cancelled = tokio::spawn(async move {
            cancelled_waiter
                .ask(SessionId::new("cancelled-session"), question(false))
                .await
        });
        let mut remaining_question = question(false);
        remaining_question.id = "question-2".to_owned();
        let remaining_waiter = Arc::clone(&broker);
        let remaining = tokio::spawn(async move {
            remaining_waiter
                .ask(SessionId::new("remaining-session"), remaining_question)
                .await
        });
        while broker.pending(None).await.len() != 2 {
            tokio::task::yield_now().await;
        }

        broker.cancel_session("cancelled-session").await;

        let error = cancelled.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("channel closed"));
        assert!(broker.pending(Some("cancelled-session")).await.is_empty());
        assert_eq!(broker.pending(Some("remaining-session")).await.len(), 1);

        let answer = UserAnswer {
            question_id: "question-2".to_owned(),
            selected: vec!["Tests".to_owned()],
            custom: None,
        };
        broker.answer(answer.clone()).await.unwrap();
        assert_eq!(remaining.await.unwrap().unwrap(), answer);
    }
}
