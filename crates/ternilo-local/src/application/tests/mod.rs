use super::*;
use ternilo_protocol::{
    GoalStatus, ProviderModelDefaults, ProviderModelReasoning, ProviderModelSettings,
    ReasoningEffort, RunLimits, SessionSubmissionRequest, SubmissionContent, SubmissionDelivery,
    SubmissionPlacement,
};

pub(super) fn test_data_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("ternilo-local-test-{}-{nonce}", std::process::id()))
}

pub(super) async fn open_test_application(data_dir: PathBuf) -> LocalApplication {
    LocalApplication::open(
        crate::catalog().unwrap(),
        crate::local_profile(),
        HostPolicy::local(RunLimits::default()),
        data_dir,
    )
    .await
    .unwrap()
}

fn queued_submission(id: &str) -> ternilo_protocol::SessionSubmission {
    ternilo_protocol::SessionSubmission {
        provenance: None,
        id: ternilo_protocol::SubmissionId::new(id),
        run_id: RunId::new(format!("run-{id}")),
        content: SubmissionContent::Prompt {
            input: id.to_owned(),
        },
        references: Vec::new(),
        attachments: Vec::new(),
        placement: SubmissionPlacement::Queued,
        created_at_ms: 10,
        updated_at_ms: 10,
    }
}

async fn run_approved_turn(
    application: &Arc<LocalApplication>,
    session_id: &str,
    content: impl Into<String>,
    expected_tool: &str,
) -> ternilo_protocol::RunOutcome {
    let content = content.into();
    let running = {
        let application = Arc::clone(application);
        let session_id = session_id.to_owned();
        tokio::spawn(async move { application.run_turn(&session_id, None, content).await })
    };
    let approval = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(question) = application
                .pending_questions(Some(session_id))
                .await
                .into_iter()
                .next()
            {
                break question;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dangerous tool requested approval");
    assert_eq!(
        approval
            .question
            .tool_approval
            .as_ref()
            .map(|context| context.tool_name.as_str()),
        Some(expected_tool)
    );
    application
        .answer_question(ternilo_protocol::UserAnswer {
            question_id: approval.question.id,
            selected: vec!["Allow once".to_owned()],
            custom: None,
        })
        .await
        .unwrap();
    running.await.unwrap().unwrap()
}

mod archive_restore;
mod code_mode;
mod commands;
mod data_layout;
mod extensions;
mod forks;
mod history;
mod interaction;
mod lifecycle;
mod models;
mod notifications;
mod presets;
mod subagents;
mod workflows;
mod workspace_tools;
