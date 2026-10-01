use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{HarnessError, SessionId, TenantId, UserId, WorkspaceId};
use ternilo_storage::{Json, Transaction, database_error, lock};

use crate::{
    ControlAction, ControlStore, ControlUser,
    store::{append_audit, require_action, to_i64},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Project,
    Workspace,
    Session,
}

impl ResourceKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Workspace => "workspace",
            Self::Session => "session",
        }
    }

    fn validate_id(self, id: &str) -> Result<(), HarnessError> {
        match self {
            Self::Project => crate::types::require_bounded(id, "project id", 128),
            Self::Workspace => WorkspaceId::new(id).validate(),
            Self::Session => SessionId::new(id).validate(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceAction {
    View,
    Submit,
    Stop,
    Configure,
    ManageSharing,
    ManageExecution,
    Delete,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each permission is an independent capability, not a mutually exclusive state."
)]
pub struct ResourcePermissions {
    pub view: bool,
    #[serde(default)]
    pub submit: bool,
    #[serde(default)]
    pub stop: bool,
    #[serde(default)]
    pub configure: bool,
}

impl ResourcePermissions {
    pub const OWNER: Self = Self {
        view: true,
        submit: true,
        stop: true,
        configure: true,
    };

    pub(crate) fn combine(&mut self, other: Self) {
        self.view |= other.view;
        self.submit |= other.submit;
        self.stop |= other.stop;
        self.configure |= other.configure;
    }

    pub(crate) const fn intersect(self, other: Self) -> Self {
        Self {
            view: self.view && other.view,
            submit: self.submit && other.submit,
            stop: self.stop && other.stop,
            configure: self.configure && other.configure,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAccessSourceKind {
    Owner,
    DirectUser,
    Group,
    Fork,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResourceAccessSource {
    pub kind: ResourceAccessSourceKind,
    pub resource_kind: ResourceKind,
    pub resource_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
    pub permissions: ResourcePermissions,
}

#[derive(Clone, Debug, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Management ownership, execution ownership, sharing and role limits are independent capabilities."
)]
pub struct ResourceAccess {
    pub sources: Vec<ResourceAccessSource>,
    pub role_limited: bool,
    pub owner_user_id: UserId,
    pub storage_user_id: UserId,
    pub ownership_revision: u64,
    pub is_execution_owner: bool,
    pub is_owner: bool,
    pub can_manage_sharing: bool,
    pub permissions: ResourcePermissions,
}

impl ResourceAccess {
    pub fn require(&self, action: ResourceAction) -> Result<(), HarnessError> {
        let allowed = match action {
            ResourceAction::View => self.permissions.view,
            ResourceAction::Submit => self.permissions.submit,
            ResourceAction::Stop => self.permissions.stop,
            ResourceAction::Configure => self.permissions.configure,
            ResourceAction::ManageSharing => self.can_manage_sharing,
            ResourceAction::ManageExecution => {
                self.is_execution_owner && self.permissions.configure
            }
            ResourceAction::Delete => self.is_owner && self.permissions.configure,
        };
        if allowed {
            Ok(())
        } else {
            Err(HarnessError::policy(
                "this resource has not been shared with the requested permission",
            ))
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShareSubject {
    User { user: ControlUser },
    Group { group: crate::GroupRecord },
}

#[derive(Clone, Debug, Serialize)]
pub struct SharedGrant {
    pub subject: ShareSubject,
    pub permissions: ResourcePermissions,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub inherited: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct GrantPage {
    pub shares: Vec<SharedGrant>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CandidatePage {
    pub candidates: Vec<ShareSubject>,
    pub next_cursor: Option<String>,
}

impl ControlStore {
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep the actor, resource, group, permissions, and audit time explicit."
    )]
    pub async fn set_resource_group_share(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        kind: ResourceKind,
        resource_id: &str,
        group_id: &str,
        permissions: Option<ResourcePermissions>,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        if permissions.is_some_and(|permissions| !permissions.view) {
            return Err(HarnessError::invalid(
                "a shared resource must allow viewing",
            ));
        }
        if kind == ResourceKind::Project {
            return self
                .set_project_group_share(
                    actor,
                    tenant_id,
                    resource_id,
                    group_id,
                    permissions,
                    now_ms,
                )
                .await;
        }
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        lock(
            &mut tx,
            &format!(
                "resource-shares:{tenant_id}:{}:{resource_id}",
                kind.as_str()
            ),
        )
        .await?;
        resource_access_in(&mut tx, &actor.user_id, tenant_id, kind, resource_id)
            .await?
            .require(ResourceAction::ManageSharing)?;
        crate::account_store::require_team_in(&mut tx, tenant_id).await?;
        crate::group_store::group_in(&mut tx, tenant_id, group_id).await?;
        if let Some(permissions) = permissions {
            let mode: String =
                sqlx::query_scalar("SELECT mode FROM control_instance_settings WHERE singleton=1")
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(database_error)?;
            if mode != "multi_user" {
                return Err(HarnessError::policy(
                    "multi-user mode is required to share resources",
                ));
            }
            sqlx::query("INSERT INTO control_resource_group_shares (tenant_id,resource_kind,resource_id,group_id,permissions_json,granted_by,created_at_ms,updated_at_ms)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$7) ON CONFLICT (tenant_id,resource_kind,resource_id,group_id) DO UPDATE SET
                permissions_json=EXCLUDED.permissions_json,granted_by=EXCLUDED.granted_by,updated_at_ms=EXCLUDED.updated_at_ms")
                .bind(tenant_id.as_str()).bind(kind.as_str()).bind(resource_id).bind(group_id).bind(Json(permissions)).bind(actor.user_id.as_str()).bind(to_i64(now_ms,"share timestamp")?)
                .execute(&mut *tx).await.map_err(database_error)?;
        } else {
            sqlx::query("DELETE FROM control_resource_group_shares WHERE tenant_id=$1 AND resource_kind=$2 AND resource_id=$3 AND group_id=$4")
                .bind(tenant_id.as_str()).bind(kind.as_str()).bind(resource_id).bind(group_id).execute(&mut *tx).await.map_err(database_error)?;
        }
        append_audit(
            &mut tx,
            tenant_id,
            Some(&actor.user_id),
            "user",
            if permissions.is_some() {
                "resource.group_share"
            } else {
                "resource.group_unshare"
            },
            kind.as_str(),
            resource_id,
            "success",
            json!({"group_id":group_id,"permissions":permissions}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn resource_share_candidates(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        kind: ResourceKind,
        resource_id: &str,
        subject_kind: &str,
        query: &crate::PageQuery,
    ) -> Result<CandidatePage, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        resource_access_in(&mut tx, &actor.user_id, tenant_id, kind, resource_id)
            .await?
            .require(ResourceAction::ManageSharing)?;
        crate::account_store::require_team_in(&mut tx, tenant_id).await?;
        let (candidates, next_cursor) = match subject_kind {
            "user" => {
                let page = crate::group_store::member_page_in(
                    &mut tx,
                    tenant_id,
                    None,
                    (kind != ResourceKind::Project).then_some(&actor.user_id),
                    query,
                )
                .await?;
                (
                    page.memberships
                        .into_iter()
                        .map(|member| ShareSubject::User {
                            user: ControlUser {
                                user_id: member.user_id,
                                username: member.username,
                            },
                        })
                        .collect(),
                    page.next_cursor,
                )
            }
            "group" => {
                let page = crate::group_store::group_page_in(&mut tx, tenant_id, query).await?;
                (
                    page.groups
                        .into_iter()
                        .map(|group| ShareSubject::Group { group })
                        .collect(),
                    page.next_cursor,
                )
            }
            _ => {
                return Err(HarnessError::invalid(
                    "sharing candidate kind must be user or group",
                ));
            }
        };
        tx.commit().await.map_err(database_error)?;
        Ok(CandidatePage {
            candidates,
            next_cursor,
        })
    }

    pub async fn resource_access(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        kind: ResourceKind,
        resource_id: &str,
    ) -> Result<ResourceAccess, HarnessError> {
        let mut transaction = self.database.tenant_transaction(tenant_id).await?;
        let access = resource_access_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            kind,
            resource_id,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(access)
    }

    pub async fn list_resource_shares(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        kind: ResourceKind,
        resource_id: &str,
        query: &crate::PageQuery,
    ) -> Result<GrantPage, HarnessError> {
        if kind == ResourceKind::Project {
            return self
                .list_project_shares(actor, tenant_id, resource_id, query)
                .await;
        }
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        resource_access_in(&mut tx, &actor.user_id, tenant_id, kind, resource_id)
            .await?
            .require(ResourceAction::ManageSharing)?;
        let rows = sqlx::query("SELECT subject_kind,subject_id,MIN(created_at_ms) AS created_at_ms,MAX(updated_at_ms) AS updated_at_ms,page_key FROM (
            SELECT 'user' AS subject_kind,s.grantee_user_id AS subject_id,s.created_at_ms,s.updated_at_ms,'user:' || s.grantee_user_id AS page_key,
                LOWER(u.username || ' ' || u.user_id) AS search_text
            FROM control_resource_shares s JOIN control_users u ON u.user_id=s.grantee_user_id
            WHERE s.tenant_id=$1 AND s.resource_kind=$2 AND s.resource_id=$3
            UNION ALL
            SELECT 'group',g.group_id,s.created_at_ms,s.updated_at_ms,'group:' || g.group_id,LOWER(g.name)
            FROM control_resource_group_shares s JOIN control_permission_groups g ON g.tenant_id=s.tenant_id AND g.group_id=s.group_id
            WHERE s.tenant_id=$1 AND s.resource_kind=$2 AND s.resource_id=$3
            UNION ALL
            SELECT 'user',f.user_id,f.created_at_ms,f.created_at_ms,'user:' || f.user_id,
                LOWER(u.username || ' ' || u.user_id)
            FROM control_resource_fork_group_sources f JOIN control_users u ON u.user_id=f.user_id
            WHERE f.tenant_id=$1 AND $2='session' AND f.session_id=$3
        ) grants WHERE (CAST($4 AS TEXT) IS NULL OR search_text LIKE $4 ESCAPE '!')
            AND (CAST($5 AS TEXT) IS NULL OR page_key>$5)
        GROUP BY subject_kind,subject_id,page_key ORDER BY page_key LIMIT $6")
            .bind(tenant_id.as_str()).bind(kind.as_str()).bind(resource_id).bind(pattern).bind(cursor).bind(i64::from(query.limit)+1)
            .fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut shares = Vec::with_capacity(rows.len());
        for row in rows {
            let subject_id: String = row.try_get("subject_id").map_err(database_error)?;
            let subject_kind: String = row.try_get("subject_kind").map_err(database_error)?;
            let (subject, permissions, inherited) = if subject_kind == "group" {
                let group = crate::group_store::group_in(&mut tx, tenant_id, &subject_id).await?;
                let permissions: Json<ResourcePermissions> = sqlx::query_scalar("SELECT permissions_json FROM control_resource_group_shares WHERE tenant_id=$1 AND resource_kind=$2 AND resource_id=$3 AND group_id=$4")
                    .bind(tenant_id.as_str()).bind(kind.as_str()).bind(resource_id).bind(&subject_id).fetch_one(&mut *tx).await.map_err(database_error)?;
                (ShareSubject::Group { group }, permissions.0, false)
            } else {
                let user_row =
                    sqlx::query("SELECT user_id,username FROM control_users WHERE user_id=$1")
                        .bind(&subject_id)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(database_error)?;
                let user = ControlUser {
                    user_id: UserId::new(&subject_id),
                    username: user_row.try_get("username").map_err(database_error)?,
                };
                let mut permissions = ResourcePermissions::default();
                let mut inherited = true;
                for source in
                    resource_sources_in(&mut tx, &user.user_id, tenant_id, kind, resource_id, None)
                        .await?
                {
                    if source.kind == ResourceAccessSourceKind::DirectUser {
                        inherited = false;
                    }
                    if matches!(
                        source.kind,
                        ResourceAccessSourceKind::DirectUser | ResourceAccessSourceKind::Fork
                    ) {
                        permissions.combine(source.permissions);
                    }
                }
                (ShareSubject::User { user }, permissions, inherited)
            };
            shares.push(SharedGrant {
                subject,
                permissions,
                inherited,
                created_at_ms: crate::store::from_i64(
                    row.try_get("created_at_ms").map_err(database_error)?,
                    "share timestamp",
                )?,
                updated_at_ms: crate::store::from_i64(
                    row.try_get("updated_at_ms").map_err(database_error)?,
                    "share timestamp",
                )?,
            });
        }
        let next_cursor = query.finish(&mut shares, |share| match &share.subject {
            ShareSubject::User { user } => format!("user:{}", user.user_id),
            ShareSubject::Group { group } => format!("group:{}", group.group_id),
        });
        tx.commit().await.map_err(database_error)?;
        Ok(GrantPage {
            shares,
            next_cursor,
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Keep the actor, resource, grantee, permissions, and audit time explicit."
    )]
    pub async fn set_resource_share(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        kind: ResourceKind,
        resource_id: &str,
        grantee: &UserId,
        permissions: Option<ResourcePermissions>,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        if permissions.is_some_and(|permissions| !permissions.view) {
            return Err(HarnessError::invalid(
                "a shared resource must allow viewing",
            ));
        }
        grantee.validate()?;
        if kind == ResourceKind::Project {
            return self
                .set_project_user_share(actor, tenant_id, resource_id, grantee, permissions, now_ms)
                .await;
        }
        let mut transaction = self.database.tenant_transaction(tenant_id).await?;
        lock(
            &mut transaction,
            &format!(
                "resource-shares:{tenant_id}:{}:{resource_id}",
                kind.as_str()
            ),
        )
        .await?;
        let access = resource_access_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            kind,
            resource_id,
        )
        .await?;
        access.require(ResourceAction::ManageSharing)?;
        if grantee == &access.owner_user_id {
            return Err(HarnessError::invalid(
                "the resource owner already has access",
            ));
        }
        if let Some(permissions) = permissions {
            let mode: String = sqlx::query_scalar(
                "SELECT mode FROM control_instance_settings WHERE singleton = 1",
            )
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
            if mode != "multi_user" {
                return Err(HarnessError::policy(
                    "multi-user mode is required to share resources",
                ));
            }
            crate::account_store::require_team_in(&mut transaction, tenant_id).await?;
            require_action(
                &mut transaction,
                tenant_id,
                grantee,
                ControlAction::TenantRead,
            )
            .await?;
            sqlx::query(
                "INSERT INTO control_resource_shares
                 (tenant_id, resource_kind, resource_id, grantee_user_id, permissions_json, granted_by, created_at_ms, updated_at_ms)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
                 ON CONFLICT (tenant_id, resource_kind, resource_id, grantee_user_id) DO UPDATE SET
                    permissions_json = EXCLUDED.permissions_json, granted_by = EXCLUDED.granted_by, updated_at_ms = EXCLUDED.updated_at_ms",
            ).bind(tenant_id.as_str()).bind(kind.as_str()).bind(resource_id).bind(grantee.as_str())
                .bind(Json(permissions)).bind(actor.user_id.as_str()).bind(to_i64(now_ms, "share timestamp")?)
                .execute(&mut *transaction).await.map_err(database_error)?;
        } else {
            sqlx::query(
                "DELETE FROM control_resource_shares WHERE tenant_id = $1 AND resource_kind = $2 AND resource_id = $3 AND grantee_user_id = $4",
            ).bind(tenant_id.as_str()).bind(kind.as_str()).bind(resource_id).bind(grantee.as_str())
                .execute(&mut *transaction).await.map_err(database_error)?;
        }
        // An owner's explicit decision replaces this child's inherited fork grant.
        // Other direct group and workspace grants remain independent sources.
        if kind == ResourceKind::Session {
            sqlx::query("DELETE FROM control_resource_fork_group_sources WHERE tenant_id=$1 AND session_id=$2 AND user_id=$3")
                .bind(tenant_id.as_str()).bind(resource_id).bind(grantee.as_str()).execute(&mut *transaction).await.map_err(database_error)?;
        }
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            if permissions.is_some() {
                "resource.share"
            } else {
                "resource.unshare"
            },
            kind.as_str(),
            resource_id,
            "success",
            json!({"grantee_user_id": grantee, "permissions": permissions}),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }
}

/// Resolve ownership and grants inside the caller's mutation transaction. The
/// caller retains the actual actor even when execution uses the owner's files.
pub async fn resource_access_in(
    transaction: &mut Transaction,
    actor_id: &UserId,
    tenant_id: &TenantId,
    kind: ResourceKind,
    resource_id: &str,
) -> Result<ResourceAccess, HarnessError> {
    kind.validate_id(resource_id)?;
    ternilo_storage::set_tenant_scope(transaction, tenant_id).await?;
    let role = require_action(transaction, tenant_id, actor_id, ControlAction::TenantRead).await?;
    if kind == ResourceKind::Project {
        return crate::project_sharing::access_in(
            transaction,
            actor_id,
            tenant_id,
            resource_id,
            role,
        )
        .await;
    }
    let (storage_owner, workspace) =
        resource_owner(transaction, tenant_id, kind, resource_id).await?;
    let (owner, ownership_revision) = crate::resource_ownership::owner_in(
        transaction,
        tenant_id,
        kind,
        resource_id,
        &storage_owner,
        workspace.as_deref(),
    )
    .await?;
    let is_owner = owner == *actor_id;
    let sources = if is_owner {
        vec![ResourceAccessSource {
            kind: ResourceAccessSourceKind::Owner,
            resource_kind: kind,
            resource_id: resource_id.to_owned(),
            resource_name: None,
            group_id: None,
            group_name: None,
            permissions: ResourcePermissions::OWNER,
        }]
    } else {
        resource_sources_in(
            transaction,
            actor_id,
            tenant_id,
            kind,
            resource_id,
            workspace.as_deref(),
        )
        .await?
    };
    let mut permissions = ResourcePermissions::default();
    for source in &sources {
        permissions.combine(source.permissions);
    }
    let original = permissions;
    if !role.allows(ControlAction::RunReserve) {
        permissions.submit = false;
        permissions.stop = false;
        permissions.configure = false;
    }
    Ok(ResourceAccess {
        owner_user_id: owner,
        is_execution_owner: storage_owner == *actor_id,
        storage_user_id: storage_owner,
        ownership_revision,
        is_owner,
        can_manage_sharing: is_owner && permissions.configure,
        permissions,
        sources,
        role_limited: permissions != original,
    })
}

pub(crate) async fn resource_sources_in(
    tx: &mut Transaction,
    actor_id: &UserId,
    tenant_id: &TenantId,
    kind: ResourceKind,
    resource_id: &str,
    workspace_id: Option<&str>,
) -> Result<Vec<ResourceAccessSource>, HarnessError> {
    let rows = sqlx::query("SELECT 'direct_user' AS source_kind,s.resource_kind,s.resource_id,CAST(NULL AS TEXT) AS group_id,CAST(NULL AS TEXT) AS group_name,s.permissions_json,CAST(NULL AS TEXT) AS current_permissions_json
        FROM control_resource_shares s WHERE s.tenant_id=$1 AND s.grantee_user_id=$2
        AND ((s.resource_kind=$3 AND s.resource_id=$4) OR (s.resource_kind='workspace' AND s.resource_id=$5))
        UNION ALL
        SELECT 'group',s.resource_kind,s.resource_id,g.group_id,g.name,s.permissions_json,CAST(NULL AS TEXT)
        FROM control_resource_group_shares s
        JOIN control_permission_groups g ON g.tenant_id=s.tenant_id AND g.group_id=s.group_id
        JOIN control_permission_group_members gm ON gm.tenant_id=s.tenant_id AND gm.group_id=s.group_id AND gm.user_id=$2
        WHERE s.tenant_id=$1 AND ((s.resource_kind=$3 AND s.resource_id=$4) OR (s.resource_kind='workspace' AND s.resource_id=$5))
        UNION ALL
        SELECT 'fork',f.source_resource_kind,f.source_resource_id,g.group_id,g.name,f.permissions_json,s.permissions_json
        FROM control_resource_fork_group_sources f
        JOIN control_resource_group_shares s ON s.tenant_id=f.tenant_id AND s.resource_kind=f.source_resource_kind AND s.resource_id=f.source_resource_id AND s.group_id=f.group_id
        JOIN control_permission_groups g ON g.tenant_id=f.tenant_id AND g.group_id=f.group_id
        JOIN control_permission_group_members gm ON gm.tenant_id=f.tenant_id AND gm.group_id=f.group_id AND gm.user_id=f.user_id
        WHERE f.tenant_id=$1 AND f.user_id=$2 AND $3='session' AND f.session_id=$4
        ORDER BY source_kind,resource_kind,resource_id,group_id")
        .bind(tenant_id.as_str()).bind(actor_id.as_str()).bind(kind.as_str()).bind(resource_id).bind(workspace_id)
        .fetch_all(&mut **tx).await.map_err(database_error)?;
    let mut sources: Vec<_> = rows
        .iter()
        .map(|row| {
            let source_kind: String = row.try_get("source_kind").map_err(database_error)?;
            let kind = match source_kind.as_str() {
                "direct_user" => ResourceAccessSourceKind::DirectUser,
                "group" => ResourceAccessSourceKind::Group,
                _ => ResourceAccessSourceKind::Fork,
            };
            let mut permissions = row
                .try_get::<Json<ResourcePermissions>, _>("permissions_json")
                .map_err(database_error)?
                .0;
            if let Some(current) = row
                .try_get::<Option<Json<ResourcePermissions>>, _>("current_permissions_json")
                .map_err(database_error)?
            {
                permissions = permissions.intersect(current.0);
            }
            let resource_kind: String = row.try_get("resource_kind").map_err(database_error)?;
            Ok(ResourceAccessSource {
                kind,
                resource_kind: if resource_kind == "workspace" {
                    ResourceKind::Workspace
                } else {
                    ResourceKind::Session
                },
                resource_id: row.try_get("resource_id").map_err(database_error)?,
                resource_name: None,
                group_id: row.try_get("group_id").map_err(database_error)?,
                group_name: row.try_get("group_name").map_err(database_error)?,
                permissions,
            })
        })
        .collect::<Result<_, HarnessError>>()?;
    let workspace = if kind == ResourceKind::Workspace {
        Some(resource_id)
    } else {
        workspace_id
    };
    if let Some(workspace) = workspace {
        sources
            .extend(crate::project_sharing::sources_in(tx, actor_id, tenant_id, workspace).await?);
    }
    Ok(sources)
}

pub(crate) async fn resource_owner(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    kind: ResourceKind,
    resource_id: &str,
) -> Result<(UserId, Option<String>), HarnessError> {
    if kind == ResourceKind::Workspace {
        let owner = sqlx::query_scalar::<_, String>(
            "SELECT owner_user_id FROM control_workspaces WHERE tenant_id = $1 AND workspace_id = $2",
        ).bind(tenant_id.as_str()).bind(resource_id).fetch_optional(&mut **transaction).await.map_err(database_error)?
            .ok_or_else(|| HarnessError::invalid("workspace does not exist"))?;
        return Ok((UserId::new(owner), None));
    }
    let edge = sqlx::query(
        "SELECT owner_user_id, workspace_id FROM control_edge_sessions WHERE tenant_id = $1 AND session_id = $2",
    ).bind(tenant_id.as_str()).bind(resource_id).fetch_optional(&mut **transaction).await.map_err(database_error)?;
    let row = if let Some(edge) = edge {
        edge
    } else {
        // A server initializes the execution schema before accepting requests.
        sqlx::query("SELECT user_id AS owner_user_id, workspace_id FROM cloud_sessions WHERE tenant_id = $1 AND session_id = $2")
            .bind(tenant_id.as_str()).bind(resource_id).fetch_optional(&mut **transaction).await.map_err(database_error)?
            .ok_or_else(|| HarnessError::invalid("session does not exist"))?
    };
    Ok((
        UserId::new(
            row.try_get::<String, _>("owner_user_id")
                .map_err(database_error)?,
        ),
        Some(row.try_get("workspace_id").map_err(database_error)?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstanceMode, NativeRegistration, OidcPrincipal, SecretCipher, TenantRole};

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Exercise grant, revocation, role, and mode transitions on the same owned resource."
    )]
    async fn sharing_is_explicit_action_scoped_and_preserves_owner() {
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([25; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "owner@example.test".to_owned(),
                    username: "owner".to_owned(),
                    password: "owner-password-123".to_owned(),
                },
                1_000,
            )
            .await
            .unwrap();
        let user = &owner.session.user;
        let team = store
            .create_tenant(
                user,
                "shared-team",
                "Shared team",
                crate::TenantQuota::default(),
                1_000,
            )
            .await
            .unwrap();
        let tenant = &team.tenant_id;
        let project = store.list_projects(user, tenant).await.unwrap().remove(0);
        let member = store
            .upsert_user(
                &OidcPrincipal {
                    issuer: "test".to_owned(),
                    subject: "member".to_owned(),
                    email: None,
                    display_name: None,
                },
                "test-member",
                1_001,
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
                1_001,
            )
            .await
            .unwrap();
        store
            .set_membership(user, tenant, &member.user_id, TenantRole::Member, 1_002)
            .await
            .unwrap();
        store
            .set_membership(user, tenant, &admin.user_id, TenantRole::Admin, 1_002)
            .await
            .unwrap();
        let workspace = store
            .create_cloud_workspace(user, tenant, &project.project_id, "Private project", 1_003)
            .await
            .unwrap();
        let kind = ResourceKind::Workspace;
        let id = workspace.workspace_id.as_str();
        for actor in [&member, &admin] {
            assert!(
                store
                    .resource_access(actor, tenant, kind, id)
                    .await
                    .unwrap()
                    .require(ResourceAction::View)
                    .is_err()
            );
        }
        let read = ResourcePermissions {
            view: true,
            ..Default::default()
        };
        assert!(
            store
                .set_resource_share(user, tenant, kind, id, &member.user_id, Some(read), 1_004)
                .await
                .is_err()
        );
        store
            .set_instance_mode(user, InstanceMode::MultiUser, 1, 1_005)
            .await
            .unwrap();
        assert!(
            store
                .set_resource_share(&admin, tenant, kind, id, &member.user_id, Some(read), 1_006)
                .await
                .is_err()
        );
        store
            .set_resource_share(user, tenant, kind, id, &member.user_id, Some(read), 1_007)
            .await
            .unwrap();
        let access = store
            .resource_access(&member, tenant, kind, id)
            .await
            .unwrap();
        assert_eq!(access.owner_user_id, user.user_id);
        assert!(!access.is_owner);
        access.require(ResourceAction::View).unwrap();
        for action in [
            ResourceAction::Submit,
            ResourceAction::Stop,
            ResourceAction::Configure,
            ResourceAction::ManageSharing,
            ResourceAction::Delete,
        ] {
            assert!(access.require(action).is_err());
        }
        let submit = ResourcePermissions {
            view: true,
            submit: true,
            ..Default::default()
        };
        store
            .set_resource_share(user, tenant, kind, id, &member.user_id, Some(submit), 1_008)
            .await
            .unwrap();
        store
            .resource_access(&member, tenant, kind, id)
            .await
            .unwrap()
            .require(ResourceAction::Submit)
            .unwrap();
        assert_eq!(
            store
                .list_resource_shares(user, tenant, kind, id, &crate::PageQuery::default())
                .await
                .unwrap()
                .shares
                .len(),
            1
        );
        store
            .set_membership(user, tenant, &member.user_id, TenantRole::Viewer, 1_009)
            .await
            .unwrap();
        assert!(
            store
                .resource_access(&member, tenant, kind, id)
                .await
                .unwrap()
                .require(ResourceAction::Submit)
                .is_err()
        );
        store
            .set_instance_mode(user, InstanceMode::SingleUser, 2, 1_010)
            .await
            .unwrap();
        assert_eq!(
            store
                .identity_session(member.clone())
                .await
                .unwrap_err()
                .message,
            "this account is paused while the server is in single-user mode"
        );
        assert_eq!(
            store
                .list_resource_shares(user, tenant, kind, id, &crate::PageQuery::default())
                .await
                .unwrap()
                .shares
                .len(),
            1
        );
        store
            .set_resource_share(user, tenant, kind, id, &member.user_id, None, 1_011)
            .await
            .unwrap();
        assert!(
            store
                .resource_access(&member, tenant, kind, id)
                .await
                .unwrap()
                .require(ResourceAction::View)
                .is_err()
        );
        assert_eq!(
            store
                .get_workspace(user, tenant, &workspace.workspace_id)
                .await
                .unwrap()
                .owner_user_id,
            user.user_id
        );
    }
}
