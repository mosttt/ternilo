use ternilo_cloud::{
    CloudCommandDelivery, CloudSessionCommandDraft, CloudSessionRecord, CloudStore,
};
use ternilo_control::{ControlStore, ControlUser, ResourceKind, ResourcePermissions};
use ternilo_protocol::{ErrorCode, WorkspaceRequest};
use ternilo_transport::{
    ApplicationOperation, CommandId, ExecutorCapability, ExecutorCommand, ExecutorCommandBody,
    ExecutorScope,
};

pub async fn assert_workspace_read_boundary(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    viewer: &ControlUser,
    session: &CloudSessionRecord,
    now: u64,
) {
    let draft = workspace_read_command(owner, session, now);
    assert_eq!(
        cloud
            .enqueue_session_command(&session.tenant_id, &viewer.user_id, &draft, now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied,
    );
    cloud
        .enqueue_session_command(&session.tenant_id, &owner.user_id, &draft, now)
        .await
        .unwrap();
    assert_eq!(
        cloud
            .session_command_as(
                &session.tenant_id,
                &viewer.user_id,
                &session.session_id,
                &draft.command.command_id
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied,
    );
    control
        .set_resource_share(
            owner,
            &session.tenant_id,
            ResourceKind::Workspace,
            session.workspace_id.as_str(),
            &viewer.user_id,
            Some(ResourcePermissions {
                view: true,
                ..ResourcePermissions::default()
            }),
            now,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .session_command_as(
                &session.tenant_id,
                &viewer.user_id,
                &session.session_id,
                &draft.command.command_id
            )
            .await
            .unwrap()
            .is_some()
    );
    let mut peer_draft = draft.clone();
    peer_draft.command.command_id = CommandId::new("workspace-peer-read");
    cloud
        .enqueue_session_command(&session.tenant_id, &viewer.user_id, &peer_draft, now)
        .await
        .unwrap();
    control
        .set_resource_share(
            owner,
            &session.tenant_id,
            ResourceKind::Workspace,
            session.workspace_id.as_str(),
            &viewer.user_id,
            None,
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        cloud
            .session_command_as(
                &session.tenant_id,
                &viewer.user_id,
                &session.session_id,
                &peer_draft.command.command_id
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied,
    );
}

fn workspace_read_command(
    owner: &ControlUser,
    session: &CloudSessionRecord,
    now: u64,
) -> CloudSessionCommandDraft {
    CloudSessionCommandDraft {
        session_id: session.session_id.clone(),
        command: ExecutorCommand {
            input_provenance: None,
            command_id: CommandId::new("workspace-file-boundary"),
            scope: ExecutorScope {
                tenant_id: session.tenant_id.clone(),
                user_id: owner.user_id.clone(),
            },
            input_authorization: None,
            issued_at_ms: now,
            expires_at_ms: now + 120_000,
            body: ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionWorkspace {
                    session_id: session.session_id.clone(),
                    request: WorkspaceRequest::Read {
                        path: "private.txt".to_owned(),
                    },
                },
            },
        },
        required_capability: ExecutorCapability::WorkspaceFiles,
        required_catalog_revision: None,
        delivery: CloudCommandDelivery::ReadOnly,
    }
}
