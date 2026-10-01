use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{Database, Transaction, database_error};

use crate::{
    ControlAction, ResourceAccess, ResourceAccessSource, ResourceAccessSourceKind, ResourceKind,
    ResourcePermissions, TenantRole,
};

mod grants;
mod inheritance;
pub use inheritance::ProjectSharingInheritance;
pub(crate) use inheritance::sources_in;

#[cfg(test)]
mod tests;

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "project_sharing",
            1,
            include_str!("schema.sql"),
            include_str!("postgres_access.sql"),
        )
        .await
}

pub(crate) async fn access_in(
    tx: &mut Transaction,
    actor: &UserId,
    tenant: &TenantId,
    project: &str,
    role: TenantRole,
) -> Result<ResourceAccess, HarnessError> {
    crate::account_store::require_team_in(tx, tenant).await?;
    let row = sqlx::query(
        "SELECT created_by,name FROM control_projects WHERE tenant_id=$1 AND project_id=$2",
    )
    .bind(tenant.as_str())
    .bind(project)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("project does not exist"))?;
    let owner = UserId::new(
        row.try_get::<String, _>("created_by")
            .map_err(database_error)?,
    );
    let is_owner = owner == *actor;
    let can_manage_sharing = role.allows(ControlAction::ProjectManage);
    let permissions = ResourcePermissions {
        view: true,
        configure: can_manage_sharing,
        ..ResourcePermissions::default()
    };
    Ok(ResourceAccess {
        storage_user_id: owner.clone(),
        ownership_revision: 0,
        is_execution_owner: is_owner,
        owner_user_id: owner,
        is_owner,
        can_manage_sharing,
        permissions,
        role_limited: false,
        sources: if is_owner {
            vec![ResourceAccessSource {
                kind: ResourceAccessSourceKind::Owner,
                resource_kind: ResourceKind::Project,
                resource_id: project.to_owned(),
                resource_name: Some(row.try_get("name").map_err(database_error)?),
                group_id: None,
                group_name: None,
                permissions,
            }]
        } else {
            vec![]
        },
    })
}
