use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{Transaction, database_error, lock};

use crate::{
    ControlAction, ControlStore, ControlUser, MembershipRecord, TenantRole,
    account_store::require_team_in,
    crypto::random_identifier,
    store::{append_audit, from_i64, require_action, to_i64},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub query: Option<String>,
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

const fn default_limit() -> u32 {
    25
}

impl Default for PageQuery {
    fn default() -> Self {
        Self {
            query: None,
            cursor: None,
            limit: default_limit(),
        }
    }
}

impl PageQuery {
    pub(crate) fn parameters(&self) -> Result<(Option<String>, Option<String>), HarnessError> {
        if !(1..=100).contains(&self.limit) {
            return Err(HarnessError::invalid(
                "page limit must be between 1 and 100",
            ));
        }
        let search = self
            .query
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if search.is_some_and(|value| value.len() > 200 || value.chars().any(char::is_control)) {
            return Err(HarnessError::invalid(
                "search must contain at most 200 bytes without control characters",
            ));
        }
        let pattern = search.map(|value| {
            format!(
                "%{}%",
                value
                    .to_lowercase()
                    .replace('!', "!!")
                    .replace('%', "!%")
                    .replace('_', "!_")
            )
        });
        let cursor = self
            .cursor
            .as_deref()
            .map(|value| {
                let invalid = || HarnessError::invalid("page cursor is invalid");
                if value.len() > 1_024 {
                    return Err(invalid());
                }
                let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| invalid())?;
                let value = String::from_utf8(bytes).map_err(|_| invalid())?;
                if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
                    return Err(invalid());
                }
                Ok(value)
            })
            .transpose()?;
        Ok((pattern, cursor))
    }

    pub(crate) fn finish<T>(
        &self,
        items: &mut Vec<T>,
        key: impl FnOnce(&T) -> String,
    ) -> Option<String> {
        let more = items.len() > self.limit as usize;
        items.truncate(self.limit as usize);
        more.then(|| items.last().map(|item| URL_SAFE_NO_PAD.encode(key(item))))
            .flatten()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupInput {
    pub name: String,
    pub description: Option<String>,
}

impl GroupInput {
    fn validate(&self) -> Result<(), HarnessError> {
        if self.name.trim().is_empty()
            || self.name.chars().count() > 120
            || self.name.chars().any(char::is_control)
        {
            return Err(HarnessError::invalid(
                "group name must contain 1 to 120 characters without control characters",
            ));
        }
        if self
            .description
            .as_ref()
            .is_some_and(|value| value.chars().count() > 2_000)
        {
            return Err(HarnessError::invalid(
                "group description must contain at most 2000 characters",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupRecord {
    pub group_id: String,
    pub tenant_id: TenantId,
    pub name: String,
    pub description: Option<String>,
    pub member_count: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupPage {
    pub groups: Vec<GroupRecord>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemberPage {
    pub memberships: Vec<MembershipRecord>,
    pub next_cursor: Option<String>,
}

pub(crate) const GROUP_COLUMNS: &str = "g.tenant_id, g.group_id, g.name, g.description, g.created_at_ms, g.updated_at_ms,
    (SELECT COUNT(*) FROM control_permission_group_members gm WHERE gm.tenant_id=g.tenant_id AND gm.group_id=g.group_id) AS member_count";

impl ControlStore {
    pub async fn list_permission_groups(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        query: &PageQuery,
    ) -> Result<GroupPage, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        require_group_manager(&mut tx, actor, tenant_id).await?;
        let page = group_page_in(&mut tx, tenant_id, query).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(page)
    }

    pub async fn permission_group(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        group_id: &str,
    ) -> Result<GroupRecord, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        require_group_manager(&mut tx, actor, tenant_id).await?;
        let group = group_in(&mut tx, tenant_id, group_id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(group)
    }

    pub async fn create_permission_group(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        input: &GroupInput,
        now_ms: u64,
    ) -> Result<GroupRecord, HarnessError> {
        input.validate()?;
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        require_group_manager(&mut tx, actor, tenant_id).await?;
        let group_id = random_identifier("group");
        sqlx::query("INSERT INTO control_permission_groups (tenant_id,group_id,name,description,created_by,created_at_ms,updated_at_ms) VALUES ($1,$2,$3,$4,$5,$6,$6)")
            .bind(tenant_id.as_str()).bind(&group_id).bind(input.name.trim()).bind(&input.description).bind(actor.user_id.as_str()).bind(to_i64(now_ms,"group timestamp")?)
            .execute(&mut *tx).await.map_err(group_name_error)?;
        audit_group(
            &mut tx,
            actor,
            tenant_id,
            &group_id,
            "permission_group.create",
            json!({"name":input.name.trim()}),
            now_ms,
        )
        .await?;
        let group = group_in(&mut tx, tenant_id, &group_id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(group)
    }

    pub async fn update_permission_group(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        group_id: &str,
        input: &GroupInput,
        now_ms: u64,
    ) -> Result<GroupRecord, HarnessError> {
        input.validate()?;
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        require_group_manager(&mut tx, actor, tenant_id).await?;
        lock_group(&mut tx, tenant_id, group_id).await?;
        group_in(&mut tx, tenant_id, group_id).await?;
        sqlx::query("UPDATE control_permission_groups SET name=$3,description=$4,updated_at_ms=$5 WHERE tenant_id=$1 AND group_id=$2")
            .bind(tenant_id.as_str()).bind(group_id).bind(input.name.trim()).bind(&input.description).bind(to_i64(now_ms,"group timestamp")?)
            .execute(&mut *tx).await.map_err(group_name_error)?;
        audit_group(
            &mut tx,
            actor,
            tenant_id,
            group_id,
            "permission_group.update",
            json!({"name":input.name.trim()}),
            now_ms,
        )
        .await?;
        let group = group_in(&mut tx, tenant_id, group_id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(group)
    }

    pub async fn delete_permission_group(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        group_id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        require_group_manager(&mut tx, actor, tenant_id).await?;
        lock_group(&mut tx, tenant_id, group_id).await?;
        group_in(&mut tx, tenant_id, group_id).await?;
        sqlx::query("DELETE FROM control_permission_groups WHERE tenant_id=$1 AND group_id=$2")
            .bind(tenant_id.as_str())
            .bind(group_id)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        audit_group(
            &mut tx,
            actor,
            tenant_id,
            group_id,
            "permission_group.delete",
            json!({}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn set_permission_group_member(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        group_id: &str,
        user_id: &UserId,
        present: bool,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        user_id.validate()?;
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        require_group_manager(&mut tx, actor, tenant_id).await?;
        lock_group(&mut tx, tenant_id, group_id).await?;
        group_in(&mut tx, tenant_id, group_id).await?;
        if present {
            require_action(&mut tx, tenant_id, user_id, ControlAction::TenantRead).await?;
            sqlx::query("INSERT INTO control_permission_group_members (tenant_id,group_id,user_id,created_at_ms) VALUES ($1,$2,$3,$4) ON CONFLICT (tenant_id,group_id,user_id) DO NOTHING")
                .bind(tenant_id.as_str()).bind(group_id).bind(user_id.as_str()).bind(to_i64(now_ms,"group member timestamp")?).execute(&mut *tx).await.map_err(database_error)?;
        } else {
            sqlx::query("DELETE FROM control_permission_group_members WHERE tenant_id=$1 AND group_id=$2 AND user_id=$3")
                .bind(tenant_id.as_str()).bind(group_id).bind(user_id.as_str()).execute(&mut *tx).await.map_err(database_error)?;
        }
        sqlx::query("UPDATE control_permission_groups SET updated_at_ms=$3 WHERE tenant_id=$1 AND group_id=$2")
            .bind(tenant_id.as_str()).bind(group_id).bind(to_i64(now_ms,"group timestamp")?).execute(&mut *tx).await.map_err(database_error)?;
        audit_group(
            &mut tx,
            actor,
            tenant_id,
            group_id,
            if present {
                "permission_group.member_add"
            } else {
                "permission_group.member_remove"
            },
            json!({"user_id":user_id}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn list_permission_group_members(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        group_id: &str,
        query: &PageQuery,
    ) -> Result<MemberPage, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        require_group_manager(&mut tx, actor, tenant_id).await?;
        group_in(&mut tx, tenant_id, group_id).await?;
        let page = member_page_in(&mut tx, tenant_id, Some(group_id), None, query).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(page)
    }

    pub async fn list_memberships(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        query: &PageQuery,
    ) -> Result<MemberPage, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        require_action(
            &mut tx,
            tenant_id,
            &actor.user_id,
            ControlAction::MembershipManage,
        )
        .await?;
        let page = member_page_in(&mut tx, tenant_id, None, None, query).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(page)
    }
}

fn group_name_error(error: sqlx::Error) -> HarnessError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        HarnessError::invalid("permission group name already exists")
    } else {
        database_error(error)
    }
}

async fn require_group_manager(
    tx: &mut Transaction,
    actor: &ControlUser,
    tenant_id: &TenantId,
) -> Result<(), HarnessError> {
    require_action(
        tx,
        tenant_id,
        &actor.user_id,
        ControlAction::MembershipManage,
    )
    .await?;
    require_team_in(tx, tenant_id).await
}

async fn lock_group(
    tx: &mut Transaction,
    tenant_id: &TenantId,
    group_id: &str,
) -> Result<(), HarnessError> {
    lock(tx, &format!("permission-group:{tenant_id}:{group_id}")).await
}

async fn audit_group(
    tx: &mut Transaction,
    actor: &ControlUser,
    tenant_id: &TenantId,
    group_id: &str,
    action: &str,
    details: serde_json::Value,
    now_ms: u64,
) -> Result<(), HarnessError> {
    append_audit(
        tx,
        tenant_id,
        Some(&actor.user_id),
        "user",
        action,
        "permission_group",
        group_id,
        "success",
        details,
        now_ms,
    )
    .await
    .map(drop)
}

pub(crate) async fn group_in(
    tx: &mut Transaction,
    tenant_id: &TenantId,
    group_id: &str,
) -> Result<GroupRecord, HarnessError> {
    UserId::new(group_id).validate()?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {GROUP_COLUMNS} FROM control_permission_groups g WHERE g.tenant_id=$1 AND g.group_id=$2")))
        .bind(tenant_id.as_str()).bind(group_id).fetch_optional(&mut **tx).await.map_err(database_error)?.ok_or_else(|| HarnessError::invalid("permission group does not exist"))?;
    group_from_row(&row)
}

pub(crate) async fn group_page_in(
    tx: &mut Transaction,
    tenant_id: &TenantId,
    query: &PageQuery,
) -> Result<GroupPage, HarnessError> {
    let (pattern, cursor) = query.parameters()?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {GROUP_COLUMNS} FROM control_permission_groups g WHERE g.tenant_id=$1
        AND (CAST($2 AS TEXT) IS NULL OR LOWER(g.name) LIKE $2 ESCAPE '!' OR LOWER(COALESCE(g.description,'')) LIKE $2 ESCAPE '!')
        AND (CAST($3 AS TEXT) IS NULL OR g.group_id>$3) ORDER BY g.group_id LIMIT $4")))
        .bind(tenant_id.as_str()).bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut **tx).await.map_err(database_error)?;
    let mut groups = rows
        .iter()
        .map(group_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = query.finish(&mut groups, |group| group.group_id.clone());
    Ok(GroupPage {
        groups,
        next_cursor,
    })
}

pub(crate) fn group_from_row(row: &AnyRow) -> Result<GroupRecord, HarnessError> {
    Ok(GroupRecord {
        group_id: row.try_get("group_id").map_err(database_error)?,
        tenant_id: TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        ),
        name: row.try_get("name").map_err(database_error)?,
        description: row.try_get("description").map_err(database_error)?,
        member_count: from_i64(
            row.try_get("member_count").map_err(database_error)?,
            "group member count",
        )?,
        created_at_ms: from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "group timestamp",
        )?,
        updated_at_ms: from_i64(
            row.try_get("updated_at_ms").map_err(database_error)?,
            "group timestamp",
        )?,
    })
}

pub(crate) async fn member_page_in(
    tx: &mut Transaction,
    tenant_id: &TenantId,
    group_id: Option<&str>,
    excluded_user: Option<&UserId>,
    query: &PageQuery,
) -> Result<MemberPage, HarnessError> {
    let (pattern, cursor) = query.parameters()?;
    let rows = sqlx::query("SELECT m.user_id,m.role,m.created_at_ms,u.username FROM control_memberships m JOIN control_users u ON u.user_id=m.user_id
        WHERE m.tenant_id=$1 AND (CAST($2 AS TEXT) IS NULL OR EXISTS(SELECT 1 FROM control_permission_group_members gm WHERE gm.tenant_id=m.tenant_id AND gm.group_id=$2 AND gm.user_id=m.user_id))
        AND (CAST($3 AS TEXT) IS NULL OR m.user_id<>$3)
        AND (CAST($4 AS TEXT) IS NULL OR LOWER(m.user_id) LIKE $4 ESCAPE '!' OR LOWER(u.username) LIKE $4 ESCAPE '!')
        AND (CAST($5 AS TEXT) IS NULL OR m.user_id>$5) ORDER BY m.user_id LIMIT $6")
        .bind(tenant_id.as_str()).bind(group_id).bind(excluded_user.map(UserId::as_str)).bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut **tx).await.map_err(database_error)?;
    let mut memberships = rows
        .iter()
        .map(|row| {
            Ok(MembershipRecord {
                user_id: UserId::new(
                    row.try_get::<String, _>("user_id")
                        .map_err(database_error)?,
                ),
                role: TenantRole::parse(
                    &row.try_get::<String, _>("role").map_err(database_error)?,
                )?,
                username: row.try_get("username").map_err(database_error)?,
                created_at_ms: from_i64(
                    row.try_get("created_at_ms").map_err(database_error)?,
                    "membership timestamp",
                )?,
            })
        })
        .collect::<Result<Vec<_>, HarnessError>>()?;
    let next_cursor = query.finish(&mut memberships, |member| {
        member.user_id.as_str().to_owned()
    });
    Ok(MemberPage {
        memberships,
        next_cursor,
    })
}

#[cfg(test)]
mod tests;
