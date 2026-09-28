use serde_json::json;
use ternilo_protocol::{
    HarnessError, SessionEvent, SessionEventKind, TenantId, UserId, UserQuestion,
    UserQuestionPresentation,
};
use ternilo_storage::Transaction;

use crate::{ControlStore, ResourceAction, ResourceKind, store::append_audit};

impl ControlStore {
    /// The caller authorizes the machine principal and supplies canonical recovery metadata.
    pub async fn record_workspace_recovery_in(
        transaction: &mut Transaction,
        tenant_id: &TenantId,
        run_id: &ternilo_protocol::RunId,
        metadata: serde_json::Value,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        append_audit(
            transaction,
            tenant_id,
            None,
            "worker",
            "workspace.recovered",
            "cloud_run",
            run_id.as_str(),
            "success",
            metadata,
            now_ms,
        )
        .await?;
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Keep the authenticated actor, owned resource and committed action explicit."
    )]
    pub async fn record_resource_action_in(
        transaction: &mut Transaction,
        actor_id: &UserId,
        tenant_id: &TenantId,
        kind: ResourceKind,
        resource_id: &str,
        owner_id: &UserId,
        action: ResourceAction,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let resource_type = kind.as_str();
        let action = match action {
            ResourceAction::View => "resource.view",
            ResourceAction::Submit => "resource.submit",
            ResourceAction::Stop => "resource.stop",
            ResourceAction::Configure => "resource.configure",
            ResourceAction::ManageSharing => "resource.sharing",
            ResourceAction::Delete => "resource.delete",
        };
        append_audit(
            transaction,
            tenant_id,
            Some(actor_id),
            "user",
            action,
            resource_type,
            resource_id,
            "success",
            json!({ "owner_user_id": owner_id }),
            now_ms,
        )
        .await?;
        Ok(())
    }
}

/// Classify the persisted question, never fields supplied by the answering client.
#[must_use]
pub fn question_resource_action(question: &UserQuestion) -> ResourceAction {
    if let Some(approval) = &question.tool_approval {
        match approval.tool_name.as_str() {
            "extension_set_enabled" | "extension_revoke" => return ResourceAction::ManageSharing,
            "extension_set_mounted" | "exit_plan_mode" => return ResourceAction::Configure,
            "interrupt_agent" | "job_kill" | "terminal_close" | "schedule_delete" => {
                return ResourceAction::Stop;
            }
            _ => {}
        }
    }
    if matches!(
        question.presentation,
        Some(UserQuestionPresentation::PlanReview { .. })
    ) {
        ResourceAction::Configure
    } else {
        ResourceAction::Submit
    }
}

/// Recognize only typed attachment fields, never a digest mentioned in text.
#[must_use]
pub fn event_references_attachment(event: &SessionEvent, digest: &str) -> bool {
    match &event.kind {
        SessionEventKind::UserMessage { attachments, .. } => attachments
            .iter()
            .any(|attachment| attachment.reference_digest() == Some(digest)),
        SessionEventKind::ToolCallFinished {
            retained_output, ..
        }
        | SessionEventKind::CodeDispatchFinished {
            retained_output, ..
        } => retained_output
            .as_ref()
            .is_some_and(|attachment| attachment.reference_digest() == Some(digest)),
        SessionEventKind::DeliverableProduced { attachment, .. } => {
            attachment.reference_digest() == Some(digest)
        }
        _ => false,
    }
}
