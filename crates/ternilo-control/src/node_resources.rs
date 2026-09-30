//! Stable server identities for resources that already exist on an owned Node.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{HarnessError, SessionId, TenantId, WorkspaceId};
use ternilo_storage::{Json, Transaction, database_error, lock};
use ternilo_transport::ExecutorId;

use crate::{
    ControlAction, ControlStore, ControlUser, EdgeSessionMetadata,
    crypto::random_identifier,
    store::{append_audit, require_action, to_i64},
    types::require_bounded,
};

pub struct NodeWorkspaceResource {
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub registered: bool,
}

pub struct NodeSessionResource {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    /// Parent IDs are Node-local on input and translated in the same transaction.
    pub metadata: EdgeSessionMetadata,
}

impl ControlStore {
    /// Discover resources only for the exact machine owner. Existing bindings,
    /// ownership, and explicit workspace removal survive every refresh.
    #[allow(clippy::too_many_lines)]
    pub async fn discover_owned_node_resources(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        workspaces: &[NodeWorkspaceResource],
        sessions: &[NodeSessionResource],
        now_ms: u64,
    ) -> Result<Vec<SessionId>, HarnessError> {
        executor_id.validate()?;
        let mut transaction = self.database.tenant_transaction(tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::TenantRead,
        )
        .await?;
        lock(
            &mut transaction,
            &format!("node-resources:{tenant_id}:{executor_id}"),
        )
        .await?;
        lock(
            &mut transaction,
            &format!("node-uploads:{tenant_id}:{executor_id}"),
        )
        .await?;
        let executor = sqlx::query(
            "SELECT project_id FROM control_executors
             WHERE tenant_id = $1 AND executor_id = $2 AND owner_user_id = $3 AND state <> 'revoked'",
        ).bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(actor.user_id.as_str())
            .fetch_optional(&mut *transaction).await.map_err(database_error)?
            .ok_or_else(|| HarnessError::policy("computer does not belong to this user"))?;
        let project_id = match executor.try_get::<Option<String>, _>("project_id").map_err(database_error)? {
            Some(id) => id,
            None => sqlx::query_scalar::<_, String>(
                "SELECT project_id FROM control_projects WHERE tenant_id = $1 ORDER BY created_at_ms, project_id LIMIT 1",
            ).bind(tenant_id.as_str()).fetch_one(&mut *transaction).await.map_err(database_error)?,
        };
        let deleted = crate::edge_store::purge_deleted_session_mappings(
            &mut transaction,
            tenant_id,
            executor_id,
        )
        .await?;
        let now = to_i64(now_ms, "Node discovery timestamp")?;
        let mut names = sqlx::query_scalar::<_, String>(
            "SELECT name FROM control_workspaces WHERE tenant_id = $1 AND owner_user_id = $2
             AND project_id = $3 AND unregistered_at_ms IS NULL",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(&project_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?
        .into_iter()
        .collect::<BTreeSet<_>>();
        let mut bindings = BTreeMap::new();
        let mut imported_workspaces = 0_u64;
        for workspace in workspaces {
            workspace.workspace_id.validate()?;
            require_bounded(&workspace.title, "workspace title", 256)?;
            if bindings.contains_key(&workspace.workspace_id) {
                return Err(HarnessError::invalid(
                    "Node snapshot contains duplicate workspaces",
                ));
            }
            let existing = sqlx::query(
                "SELECT workspace_id, owner_user_id FROM control_workspaces
                 WHERE tenant_id = $1 AND executor_id = $2 AND executor_workspace_id = $3",
            )
            .bind(tenant_id.as_str())
            .bind(executor_id.as_str())
            .bind(workspace.workspace_id.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?;
            let public_id = if let Some(row) = existing {
                if row
                    .try_get::<String, _>("owner_user_id")
                    .map_err(database_error)?
                    != actor.user_id.as_str()
                {
                    return Err(HarnessError::policy("workspace binding has another owner"));
                }
                WorkspaceId::new(
                    row.try_get::<String, _>("workspace_id")
                        .map_err(database_error)?,
                )
            } else {
                let id = WorkspaceId::new(random_identifier("wsp"));
                let name = available_name(&workspace.title, &mut names);
                sqlx::query(
                    "INSERT INTO control_workspaces
                     (tenant_id, workspace_id, project_id, owner_user_id, name, placement, storage,
                      executor_id, executor_workspace_id, created_at_ms, updated_at_ms, unregistered_at_ms)
                     VALUES ($1, $2, $3, $4, $5, 'local_node', 'local_path', $6, $7, $8, $8, $9)",
                ).bind(tenant_id.as_str()).bind(id.as_str()).bind(&project_id).bind(actor.user_id.as_str())
                    .bind(name).bind(executor_id.as_str()).bind(workspace.workspace_id.as_str())
                    .bind(now).bind((!workspace.registered).then_some(now))
                    .execute(&mut *transaction).await.map_err(database_error)?;
                imported_workspaces += 1;
                id
            };
            bindings.insert(workspace.workspace_id.clone(), public_id);
        }
        let existing = sqlx::query(
            "SELECT session_id, node_session_id, workspace_id, owner_user_id FROM control_edge_sessions
             WHERE tenant_id = $1 AND executor_id = $2",
        ).bind(tenant_id.as_str()).bind(executor_id.as_str())
            .fetch_all(&mut *transaction).await.map_err(database_error)?;
        let mut identities = BTreeMap::new();
        for row in existing {
            if row
                .try_get::<String, _>("owner_user_id")
                .map_err(database_error)?
                != actor.user_id.as_str()
            {
                return Err(HarnessError::policy("session binding has another owner"));
            }
            identities.insert(
                SessionId::new(
                    row.try_get::<String, _>("node_session_id")
                        .map_err(database_error)?,
                ),
                (
                    SessionId::new(
                        row.try_get::<String, _>("session_id")
                            .map_err(database_error)?,
                    ),
                    WorkspaceId::new(
                        row.try_get::<String, _>("workspace_id")
                            .map_err(database_error)?,
                    ),
                    false,
                ),
            );
        }
        let mut seen = BTreeSet::new();
        for session in sessions {
            session.session_id.validate()?;
            session.metadata.validate()?;
            if deleted.contains(&session.session_id) {
                continue;
            }
            if !seen.insert(session.session_id.clone()) {
                return Err(HarnessError::invalid(
                    "Node snapshot contains duplicate sessions",
                ));
            }
            let workspace = bindings
                .get(&session.workspace_id)
                .ok_or_else(|| HarnessError::invalid("Node session has no workspace binding"))?;
            if let Some((_, existing_workspace, _)) = identities.get(&session.session_id) {
                if existing_workspace != workspace {
                    return Err(HarnessError::policy(
                        "Node changed an immutable session workspace binding",
                    ));
                }
            } else {
                identities.insert(
                    session.session_id.clone(),
                    (
                        SessionId::new(random_identifier("ses")),
                        workspace.clone(),
                        true,
                    ),
                );
            }
        }
        let mut imported_sessions = Vec::new();
        for session in sessions {
            if deleted.contains(&session.session_id) {
                continue;
            }
            let (public_id, workspace_id, is_new) = &identities[&session.session_id];
            crate::EdgeStore::record_session_provenance_context_in_transaction(
                &mut transaction,
                tenant_id,
                executor_id,
                &session.session_id,
                session.metadata.parent_session_id.as_ref(),
                session
                    .metadata
                    .subagent
                    .as_ref()
                    .map(|subagent| &subagent.subagent_id),
            )
            .await?;
            let mut metadata = session.metadata.clone();
            metadata.server_model = None;
            metadata.parent_session_id = metadata
                .parent_session_id
                .as_ref()
                .and_then(|parent| identities.get(parent).map(|(id, _, _)| id.clone()));
            if *is_new {
                if let Some(parent) = &metadata.parent_session_id {
                    let inherited: Option<Json<EdgeSessionMetadata>> = sqlx::query_scalar("SELECT metadata_json FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2")
                        .bind(tenant_id.as_str()).bind(parent.as_str()).fetch_optional(&mut *transaction).await.map_err(database_error)?;
                    metadata.server_model = inherited.and_then(|metadata| metadata.0.server_model);
                }
                sqlx::query(
                    "INSERT INTO control_edge_sessions
                     (tenant_id, session_id, workspace_id, executor_id, owner_user_id,
                      node_session_id, metadata_json, last_event_seq, created_at_ms, updated_at_ms)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, $8, $8)",
                )
                .bind(tenant_id.as_str())
                .bind(public_id.as_str())
                .bind(workspace_id.as_str())
                .bind(executor_id.as_str())
                .bind(actor.user_id.as_str())
                .bind(session.session_id.as_str())
                .bind(Json(&metadata))
                .bind(now)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
                imported_sessions.push(public_id.clone());
            } else {
                update_metadata(&mut transaction, tenant_id, public_id, &metadata, now).await?;
            }
        }
        if imported_workspaces != 0 || !imported_sessions.is_empty() {
            append_audit(
                &mut transaction,
                tenant_id,
                Some(&actor.user_id),
                "user",
                "node.resources.discover",
                "executor",
                executor_id.as_str(),
                "success",
                json!({ "workspaces": imported_workspaces, "sessions": imported_sessions.len() }),
                now_ms,
            )
            .await?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(imported_sessions)
    }
}

async fn update_metadata(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    session_id: &SessionId,
    metadata: &EdgeSessionMetadata,
    now: i64,
) -> Result<(), HarnessError> {
    let existing: Json<EdgeSessionMetadata> = sqlx::query_scalar(ternilo_storage::for_update(transaction,
        "SELECT metadata_json FROM control_edge_sessions WHERE tenant_id = $1 AND session_id = $2",
        "SELECT metadata_json FROM control_edge_sessions WHERE tenant_id = $1 AND session_id = $2 FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .bind(session_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    let mut metadata = metadata.clone();
    metadata.server_model = existing.0.server_model.clone();
    if metadata.updated_at_ms >= existing.0.updated_at_ms && metadata != existing.0 {
        sqlx::query(
            "UPDATE control_edge_sessions SET metadata_json = $3, updated_at_ms = $4
             WHERE tenant_id = $1 AND session_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(Json(&metadata))
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

fn available_name(title: &str, names: &mut BTreeSet<String>) -> String {
    if names.insert(title.to_owned()) {
        return title.to_owned();
    }
    let base = title
        .char_indices()
        .take_while(|(index, _)| *index < 200)
        .map(|(_, character)| character)
        .collect::<String>();
    for number in 2_u64.. {
        let candidate = format!("{base} ({number})");
        if names.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("finite workspace names cannot exhaust the suffix space")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OidcPrincipal, SecretCipher, TenantQuota, TenantRole};
    use std::time::Duration;
    use ternilo_protocol::{PermissionPreset, SessionMode};

    fn session(id: &str, parent: Option<&str>) -> NodeSessionResource {
        NodeSessionResource {
            session_id: SessionId::new(id),
            workspace_id: WorkspaceId::new("same-local-workspace"),
            metadata: EdgeSessionMetadata {
                server_model: None,
                parent_session_id: parent.map(SessionId::new),
                subagent: None,
                title: id.to_owned(),
                archived_at_ms: None,
                blank: false,
                permissions: PermissionPreset::WorkspaceWrite,
                model: json!({"kind": "profile_default"}),
                agent_preset: "standard".to_owned(),
                preset_plugins: vec![],
                profile_plugins: vec![],
                mode: SessionMode::Execute,
                created_at_ms: 100,
                updated_at_ms: 100,
            },
        }
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep two-machine discovery and stable identity checks in one lifecycle scenario."
    )]
    async fn discovery_preserves_machine_identity_owner_and_ungrouped_sessions() {
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([19; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .upsert_user(
                &OidcPrincipal {
                    issuer: "test".to_owned(),
                    subject: "owner".to_owned(),
                    email: None,
                    display_name: None,
                },
                "test-owner",
                100,
            )
            .await
            .unwrap();
        let admin = store
            .upsert_user(
                &OidcPrincipal {
                    issuer: "test".to_owned(),
                    subject: "admin".to_owned(),
                    email: None,
                    display_name: None,
                },
                "test-admin",
                100,
            )
            .await
            .unwrap();
        let tenant = store
            .create_tenant(
                &owner,
                "discovery",
                "Discovery",
                TenantQuota::default(),
                100,
            )
            .await
            .unwrap()
            .tenant_id;
        store
            .set_membership(&owner, &tenant, &admin.user_id, TenantRole::Admin, 101)
            .await
            .unwrap();
        let nodes = [ExecutorId::new("laptop"), ExecutorId::new("vps")];
        for node in &nodes {
            let grant = store
                .create_owned_enrollment(
                    &owner,
                    &tenant,
                    None,
                    node.clone(),
                    Duration::from_secs(60),
                    102,
                )
                .await
                .unwrap();
            store.consume_enrollment(&grant.token, 103).await.unwrap();
        }
        let workspaces = [NodeWorkspaceResource {
            workspace_id: WorkspaceId::new("same-local-workspace"),
            title: "Project".to_owned(),
            registered: true,
        }];
        let sessions = [session("child", Some("parent")), session("parent", None)];
        for node in &nodes {
            assert_eq!(
                store
                    .discover_owned_node_resources(
                        &owner,
                        &tenant,
                        node,
                        &workspaces,
                        &sessions,
                        110,
                    )
                    .await
                    .unwrap()
                    .len(),
                2
            );
        }
        let registered = store.list_workspaces(&owner, &tenant).await.unwrap();
        let first = registered
            .iter()
            .find(|item| item.executor_id.as_ref() == Some(&nodes[0]))
            .unwrap();
        let second = registered
            .iter()
            .find(|item| item.executor_id.as_ref() == Some(&nodes[1]))
            .unwrap();
        let collision = store
            .create_local_workspace(
                &owner,
                &tenant,
                &second.project_id,
                &first.name,
                (&nodes[1], &workspaces[0].workspace_id),
                111,
            )
            .await
            .unwrap_err();
        assert_eq!(collision.code, ternilo_protocol::ErrorCode::Conflict);
        assert_eq!(
            collision.message,
            "workspace name already exists in this project"
        );
        let collision = store
            .rename_owned_workspace(&owner, &tenant, &second.workspace_id, &first.name, 111)
            .await
            .unwrap_err();
        assert_eq!(collision.code, ternilo_protocol::ErrorCode::Conflict);
        let reopened = store
            .create_local_workspace(
                &owner,
                &tenant,
                &second.project_id,
                "Project (vps)",
                (&nodes[1], &workspaces[0].workspace_id),
                111,
            )
            .await
            .unwrap();
        assert_eq!(reopened.workspace_id, second.workspace_id);
        assert_eq!(reopened.executor_id.as_ref(), Some(&nodes[1]));
        assert_eq!(
            store.list_workspaces(&owner, &tenant).await.unwrap().len(),
            2
        );
        let mappings = store
            .list_owned_edge_sessions(&owner, &tenant)
            .await
            .unwrap();
        assert_eq!(mappings.len(), 4);
        assert_eq!(
            mappings
                .iter()
                .map(|mapping| &mapping.session_id)
                .collect::<BTreeSet<_>>()
                .len(),
            4
        );
        for child in mappings
            .iter()
            .filter(|mapping| mapping.node_session_id.as_str() == "child")
        {
            let parent = mappings
                .iter()
                .find(|mapping| {
                    mapping.executor_id == child.executor_id
                        && mapping.node_session_id.as_str() == "parent"
                })
                .unwrap();
            assert_eq!(
                child.metadata.parent_session_id.as_ref(),
                Some(&parent.session_id)
            );
            assert_eq!(child.workspace_id, parent.workspace_id);
        }
        let forged = [session("child", None), session("parent", None)];
        assert!(
            store
                .discover_owned_node_resources(
                    &owner,
                    &tenant,
                    &nodes[0],
                    &workspaces,
                    &forged,
                    111
                )
                .await
                .is_err(),
            "discovery cannot rewrite an existing session's provenance lineage"
        );
        let unchanged = store
            .list_owned_edge_sessions(&owner, &tenant)
            .await
            .unwrap();
        assert_eq!(unchanged, mappings);
        let before = mappings
            .iter()
            .map(|mapping| mapping.session_id.clone())
            .collect::<BTreeSet<_>>();
        assert!(
            store
                .discover_owned_node_resources(
                    &admin,
                    &tenant,
                    &nodes[0],
                    &workspaces,
                    &sessions,
                    111,
                )
                .await
                .is_err()
        );
        let workspace = store
            .list_workspaces(&owner, &tenant)
            .await
            .unwrap()
            .into_iter()
            .find(|workspace| workspace.executor_id.as_ref() == Some(&nodes[0]))
            .unwrap();
        store
            .unregister_owned_workspace(&owner, &tenant, &workspace.workspace_id, 112)
            .await
            .unwrap();
        assert!(
            store
                .discover_owned_node_resources(
                    &owner,
                    &tenant,
                    &nodes[0],
                    &workspaces,
                    &sessions,
                    113,
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.list_workspaces(&owner, &tenant).await.unwrap().len(),
            1
        );
        assert_eq!(
            store
                .list_owned_edge_sessions(&owner, &tenant)
                .await
                .unwrap()
                .iter()
                .map(|mapping| mapping.session_id.clone())
                .collect::<BTreeSet<_>>(),
            before
        );
        let restored = store
            .create_local_workspace(
                &owner,
                &tenant,
                &workspace.project_id,
                "Restored project",
                (&nodes[0], &workspaces[0].workspace_id),
                114,
            )
            .await
            .unwrap();
        assert_eq!(restored.workspace_id, workspace.workspace_id);
        assert_eq!(
            store.list_workspaces(&owner, &tenant).await.unwrap().len(),
            2
        );
        let mut invalid = sessions;
        invalid[0].workspace_id = WorkspaceId::new("missing");
        assert!(
            store
                .discover_owned_node_resources(
                    &owner,
                    &tenant,
                    &nodes[0],
                    &workspaces,
                    &invalid,
                    114,
                )
                .await
                .is_err()
        );
        assert_eq!(
            store
                .list_owned_edge_sessions(&owner, &tenant)
                .await
                .unwrap()
                .len(),
            4
        );
        store
            .edge_store()
            .begin_upload_sync(
                &nodes[0],
                &ternilo_transport::ExecutorScope {
                    tenant_id: tenant.clone(),
                    user_id: owner.user_id.clone(),
                },
                "1234567890abcdef1234567890abcdef",
            )
            .await
            .unwrap();
        let mut transaction = store.database.tenant_transaction(&tenant).await.unwrap();
        sqlx::query("INSERT INTO control_edge_deleted_sessions (tenant_id,executor_id,session_id) VALUES ($1,$2,'parent')")
            .bind(tenant.as_str()).bind(nodes[0].as_str()).execute(&mut *transaction).await.unwrap();
        transaction.commit().await.unwrap();
        assert!(
            store
                .discover_owned_node_resources(
                    &owner,
                    &tenant,
                    &nodes[0],
                    &workspaces,
                    &[session("child", Some("parent")), session("parent", None)],
                    115,
                )
                .await
                .unwrap()
                .is_empty()
        );
        let remaining = store
            .list_owned_edge_sessions(&owner, &tenant)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 3);
        assert!(
            !remaining
                .iter()
                .any(|mapping| mapping.executor_id == nodes[0]
                    && mapping.node_session_id.as_str() == "parent")
        );
        assert_eq!(
            remaining
                .iter()
                .filter(|mapping| mapping.executor_id == nodes[1])
                .count(),
            2
        );
    }
}
