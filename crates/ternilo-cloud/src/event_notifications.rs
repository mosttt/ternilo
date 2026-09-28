use std::{collections::HashSet, sync::Arc, time::Duration};

use serde::Deserialize;
use sqlx::postgres::PgListener;
use ternilo_protocol::{HarnessError, SessionId, SessionLiveDirty, TenantId, UserId};
use ternilo_storage::{Backend, Database};
use tokio::sync::{broadcast, watch};

const EVENT_CHANNEL: &str = "ternilo_cloud_session_events";
const LIVE_CHANNEL: &str = "ternilo_cloud_live";
const BROADCAST_CAPACITY: usize = 1_024;
const CHANGE_BATCH_SIZE: u16 = 256;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct SessionEventNotification {
    tenant_id: String,
    #[serde(default)]
    user_id: Option<String>,
    session_id: String,
    event_type: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct LiveInvalidationNotification {
    kind: String,
    tenant_id: String,
    user_id: String,
    session_id: Option<String>,
}

/// A wake-up hint from database changes or local credential revocation. Consumers
/// must re-read canonical rows after notifications, rescans, or receiver lag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CloudLiveNotification {
    Session {
        tenant_id: TenantId,
        user_id: Option<UserId>,
        session_id: Option<SessionId>,
        dirty: SessionLiveDirty,
        activity: bool,
        workbench: bool,
    },
    Workbench {
        tenant_id: TenantId,
        user_id: UserId,
    },
    Reauthenticate {
        user_id: UserId,
    },
    Rescan,
}

#[derive(Debug)]
struct ListenerLifetime {
    shutdown: watch::Sender<bool>,
}

impl Drop for ListenerLifetime {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
    }
}

/// One database feed shared by event and live metadata waiters in a Server.
#[derive(Clone, Debug)]
pub struct CloudSessionEventFeed {
    sender: broadcast::Sender<CloudLiveNotification>,
    _lifetime: Arc<ListenerLifetime>,
}

impl CloudSessionEventFeed {
    pub async fn connect(database_url: &str) -> Result<Self, HarnessError> {
        Self::from_database(Database::connect(database_url, 2).await?).await
    }

    pub async fn from_database(database: Database) -> Result<Self, HarnessError> {
        let mut listener = if database.backend() == Backend::Postgres {
            let mut listener =
                PgListener::connect(database.pool().connect_options().database_url.as_str())
                    .await
                    .map_err(|error| listener_error(&error))?;
            listener
                .listen(EVENT_CHANNEL)
                .await
                .map_err(|error| listener_error(&error))?;
            listener
                .listen(LIVE_CHANNEL)
                .await
                .map_err(|error| listener_error(&error))?;
            listener.eager_reconnect(true);
            Some(listener)
        } else {
            None
        };
        // New subscriptions read canonical state first. Only later changes need hints.
        let sequence = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(sequence), 0) FROM cloud_live_changes",
        )
        .fetch_one(database.pool())
        .await
        .map_err(|error| listener_error(&error))?;
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        let (shutdown, mut shutdown_receiver) = watch::channel(false);
        let task_sender = sender.clone();
        tokio::spawn(async move {
            let mut cursor = ChangeCursor {
                sequence,
                notified: HashSet::new(),
            };
            let interval = if database.backend() == Backend::Sqlite {
                100
            } else {
                500
            };
            let mut polling = tokio::time::interval(Duration::from_millis(interval));
            polling.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut query_failed = false;
            let mut listener_failed = false;
            loop {
                tokio::select! {
                    changed = shutdown_receiver.changed() => {
                        if changed.is_err() || *shutdown_receiver.borrow() { break; }
                    }
                    notification = async {
                        match listener.as_mut() {
                            Some(listener) => listener.try_recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        match notification {
                            Ok(Some(notification)) => {
                                listener_failed = false;
                                if let Some((sequence, signal)) = decode_notification(
                                    notification.channel(), notification.payload(),
                                ) && cursor.accept_notification(sequence) {
                                    let _ = task_sender.send(signal);
                                }
                            }
                            Ok(None) => {
                                // Reconnection may have lost a late commit below the scan cursor.
                                let _ = task_sender.send(CloudLiveNotification::Rescan);
                                listener_failed = false;
                            }
                            Err(_) => {
                                if !listener_failed {
                                    let _ = task_sender.send(CloudLiveNotification::Rescan);
                                    listener_failed = true;
                                }
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                    _ = polling.tick() => {
                        match read_changes(&database, &mut cursor, &task_sender).await {
                            Ok(has_more) => {
                                query_failed = false;
                                if has_more { polling.reset_immediately(); }
                            }
                            Err(_) if !query_failed => {
                                query_failed = true;
                                let _ = task_sender.send(CloudLiveNotification::Rescan);
                            }
                            Err(_) => {},
                        }
                    }
                }
            }
        });
        Ok(Self {
            sender,
            _lifetime: Arc::new(ListenerLifetime { shutdown }),
        })
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<CloudLiveNotification> {
        self.sender.subscribe()
    }

    /// Prompt this Server's live connections to revalidate one account.
    pub fn reauthenticate_user(&self, user_id: &UserId) {
        let _ = self.sender.send(CloudLiveNotification::Reauthenticate {
            user_id: user_id.clone(),
        });
    }
}

#[derive(Default)]
struct ChangeCursor {
    sequence: i64,
    notified: HashSet<i64>,
}

impl ChangeCursor {
    fn accept_notification(&mut self, sequence: Option<i64>) -> bool {
        // PostgreSQL sequence allocation is not commit order. A late commit below
        // the polling cursor must still invalidate its exact scope.
        sequence.is_none_or(|sequence| sequence <= self.sequence || self.notified.insert(sequence))
    }
}

#[derive(sqlx::FromRow)]
struct LiveChange {
    sequence: i64,
    kind: String,
    tenant_id: String,
    user_id: Option<String>,
    session_id: Option<String>,
    event_type: Option<String>,
}

impl LiveChange {
    fn signal(self) -> Option<CloudLiveNotification> {
        if self.kind == "event" {
            Some(event_signal(SessionEventNotification {
                tenant_id: self.tenant_id,
                user_id: self.user_id,
                session_id: self.session_id?,
                event_type: self.event_type.unwrap_or_default(),
            }))
        } else {
            invalidation_signal(LiveInvalidationNotification {
                kind: self.kind,
                tenant_id: self.tenant_id,
                user_id: self.user_id?,
                session_id: self.session_id,
            })
        }
    }
}

async fn read_changes(
    database: &Database,
    cursor: &mut ChangeCursor,
    sender: &broadcast::Sender<CloudLiveNotification>,
) -> Result<bool, sqlx::Error> {
    let changes = sqlx::query_as::<_, LiveChange>(
        "SELECT sequence, kind, tenant_id, user_id, session_id, event_type
         FROM cloud_live_changes WHERE sequence > $1 ORDER BY sequence LIMIT $2",
    )
    .bind(cursor.sequence)
    .bind(i64::from(CHANGE_BATCH_SIZE))
    .fetch_all(database.pool())
    .await?;
    let has_more = changes.len() == usize::from(CHANGE_BATCH_SIZE);
    for change in changes {
        cursor.sequence = change.sequence;
        if !cursor.notified.remove(&change.sequence)
            && let Some(signal) = change.signal()
        {
            let _ = sender.send(signal);
        }
    }
    Ok(has_more)
}

fn decode_notification(
    channel: &str,
    payload: &str,
) -> Option<(Option<i64>, CloudLiveNotification)> {
    let signal = match channel {
        EVENT_CHANNEL => serde_json::from_str::<SessionEventNotification>(payload)
            .ok()
            .map(event_signal),
        LIVE_CHANNEL => serde_json::from_str::<LiveInvalidationNotification>(payload)
            .ok()
            .and_then(invalidation_signal),
        _ => None,
    }?;
    let sequence = serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("sequence")
        .and_then(serde_json::Value::as_i64);
    Some((sequence, signal))
}

fn event_signal(notification: SessionEventNotification) -> CloudLiveNotification {
    let event_type = notification.event_type.as_str();
    let streaming_delta = matches!(
        event_type,
        "assistant_message_delta" | "assistant_reasoning_delta"
    );
    CloudLiveNotification::Session {
        tenant_id: TenantId::new(notification.tenant_id),
        user_id: notification.user_id.map(UserId::new),
        session_id: Some(SessionId::new(notification.session_id)),
        dirty: SessionLiveDirty {
            events: true,
            stats: !streaming_delta,
            projection: !streaming_delta,
            questions: matches!(event_type, "user_question_asked" | "user_question_answered"),
            profile: matches!(
                event_type,
                "runtime_extension_changed" | "plan_review_completed"
            ),
            agent_team: matches!(event_type, "subagent_updated" | "session_title_generated"),
            ..SessionLiveDirty::default()
        },
        activity: matches!(
            event_type,
            "turn_started"
                | "execution_activity_changed"
                | "workspace_execution_waiting"
                | "workspace_execution_acquired"
                | "turn_finished"
                | "turn_failed"
                | "turn_cancelled"
        ),
        workbench: false,
    }
}

fn invalidation_signal(
    notification: LiveInvalidationNotification,
) -> Option<CloudLiveNotification> {
    let tenant_id = TenantId::new(notification.tenant_id);
    let user_id = UserId::new(notification.user_id);
    let session_id = notification.session_id.map(SessionId::new);
    match notification.kind.as_str() {
        "inbox" => Some(CloudLiveNotification::Session {
            tenant_id,
            user_id: Some(user_id),
            session_id,
            dirty: SessionLiveDirty {
                inbox: true,
                ..SessionLiveDirty::default()
            },
            activity: false,
            workbench: false,
        }),
        "agent_team" => Some(CloudLiveNotification::Session {
            tenant_id,
            user_id: Some(user_id),
            session_id,
            dirty: SessionLiveDirty {
                agent_team: true,
                ..SessionLiveDirty::default()
            },
            activity: false,
            workbench: false,
        }),
        "questions" => Some(CloudLiveNotification::Session {
            tenant_id,
            user_id: Some(user_id),
            session_id,
            dirty: SessionLiveDirty {
                questions: true,
                ..SessionLiveDirty::default()
            },
            activity: false,
            workbench: false,
        }),
        "activity" => Some(CloudLiveNotification::Session {
            tenant_id,
            user_id: Some(user_id),
            session_id,
            dirty: SessionLiveDirty::default(),
            activity: true,
            workbench: false,
        }),
        "session" => Some(CloudLiveNotification::Session {
            tenant_id,
            user_id: Some(user_id),
            session_id,
            dirty: SessionLiveDirty {
                profile: true,
                agent_team: true,
                ..SessionLiveDirty::default()
            },
            activity: false,
            workbench: true,
        }),
        "workbench" => Some(CloudLiveNotification::Workbench { tenant_id, user_id }),
        _ => None,
    }
}

fn listener_error(error: &sqlx::Error) -> HarnessError {
    HarnessError::execution(format!("cloud event listener: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_late_commits_below_the_scan_cursor_still_wake_readers() {
        let mut cursor = ChangeCursor::default();
        assert!(cursor.accept_notification(Some(2)));
        assert!(!cursor.accept_notification(Some(2)));
        cursor.sequence = 2;
        cursor.notified.remove(&2);
        assert!(cursor.accept_notification(Some(1)));
        assert!(cursor.accept_notification(None));
    }

    #[test]
    fn persistent_notifications_keep_the_legacy_payload_contract() {
        let payload = serde_json::json!({
            "sequence": 12,
            "tenant_id": "tenant",
            "user_id": "user",
            "session_id": "session",
            "event_type": "assistant_message_delta",
        });
        let (sequence, signal) = decode_notification(EVENT_CHANNEL, &payload.to_string()).unwrap();
        assert_eq!(sequence, Some(12));
        assert!(
            matches!(signal, CloudLiveNotification::Session { dirty, workbench: false, .. }
            if dirty.events && dirty.metadata().is_empty())
        );
        assert!(decode_notification(EVENT_CHANNEL, "{}").is_none());
        assert!(decode_notification("unknown", &payload.to_string()).is_none());
    }

    #[test]
    fn live_payloads_map_to_precise_dirty_slices() {
        let inbox = invalidation_signal(LiveInvalidationNotification {
            kind: "inbox".to_owned(),
            tenant_id: "tenant".to_owned(),
            user_id: "user".to_owned(),
            session_id: Some("session".to_owned()),
        })
        .expect("known live notification");
        assert!(matches!(
            inbox,
            CloudLiveNotification::Session { dirty, workbench: false, .. }
                if dirty.inbox && !dirty.events && !dirty.agent_team
        ));

        let questions = invalidation_signal(LiveInvalidationNotification {
            kind: "questions".to_owned(),
            tenant_id: "tenant".to_owned(),
            user_id: "user".to_owned(),
            session_id: Some("session".to_owned()),
        })
        .expect("known live notification");
        assert!(matches!(
            questions,
            CloudLiveNotification::Session { dirty, workbench: false, .. }
                if dirty.questions && !dirty.events && !dirty.inbox
        ));

        let session = invalidation_signal(LiveInvalidationNotification {
            kind: "session".to_owned(),
            tenant_id: "tenant".to_owned(),
            user_id: "user".to_owned(),
            session_id: Some("session".to_owned()),
        })
        .expect("known live notification");
        assert!(matches!(
            session,
            CloudLiveNotification::Session { dirty, workbench: true, .. }
                if dirty.profile && dirty.agent_team
        ));

        let activity = invalidation_signal(LiveInvalidationNotification {
            kind: "activity".to_owned(),
            tenant_id: "tenant".to_owned(),
            user_id: "user".to_owned(),
            session_id: Some("session".to_owned()),
        })
        .expect("known live notification");
        assert!(matches!(
            activity,
            CloudLiveNotification::Session {
                dirty,
                activity: true,
                workbench: false,
                ..
            } if dirty.is_empty()
        ));
    }

    #[test]
    fn streaming_event_notifications_only_dirty_the_journal() {
        for event_type in ["assistant_message_delta", "assistant_reasoning_delta"] {
            let signal = event_signal(SessionEventNotification {
                tenant_id: "tenant".to_owned(),
                user_id: Some("user".to_owned()),
                session_id: "session".to_owned(),
                event_type: event_type.to_owned(),
            });
            assert!(matches!(
                signal,
                CloudLiveNotification::Session {
                    dirty,
                    activity: false,
                    workbench: false,
                    ..
                } if dirty.events && dirty.metadata().is_empty()
            ));
        }

        let question = event_signal(SessionEventNotification {
            tenant_id: "tenant".to_owned(),
            user_id: Some("user".to_owned()),
            session_id: "session".to_owned(),
            event_type: "user_question_asked".to_owned(),
        });
        assert!(matches!(
            question,
            CloudLiveNotification::Session { dirty, .. }
                if dirty.events && dirty.stats && dirty.projection && dirty.questions
        ));
    }
}
