use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Transaction, database_error, lock};

use super::{
    ControlStore, ModelGroupMemberPage, ModelGroupPage, ModelGroupRecord, PlatformAction, number,
    read_number, validate_text,
};
use crate::{
    ControlUser, GroupInput, PageQuery, account_store::append_platform_audit,
    crypto::random_identifier,
};

impl ControlStore {
    pub async fn list_model_groups(
        &self,
        actor: &ControlUser,
        query: &PageQuery,
    ) -> Result<ModelGroupPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsRead)
            .await?;
        let rows=sqlx::query("SELECT g.*,(SELECT COUNT(*) FROM control_model_group_members m WHERE m.group_id=g.group_id) AS member_count FROM control_model_groups g WHERE deleted_at_ms IS NULL AND (CAST($1 AS TEXT) IS NULL OR LOWER(g.name) LIKE $1 ESCAPE '!' OR LOWER(COALESCE(g.description,'')) LIKE $1 ESCAPE '!') AND (CAST($2 AS TEXT) IS NULL OR g.group_id>$2) ORDER BY g.group_id LIMIT $3")
            .bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut groups = rows
            .iter()
            .map(group_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = query.finish(&mut groups, |group| group.group_id.clone());
        tx.commit().await.map_err(database_error)?;
        Ok(ModelGroupPage {
            groups,
            next_cursor,
        })
    }

    pub async fn get_model_group(
        &self,
        actor: &ControlUser,
        id: &str,
    ) -> Result<ModelGroupRecord, HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsRead)
            .await?;
        let group = group_in(&mut tx, id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(group)
    }

    pub async fn save_model_group(
        &self,
        actor: &ControlUser,
        id: Option<&str>,
        input: &GroupInput,
        now_ms: u64,
    ) -> Result<ModelGroupRecord, HarnessError> {
        validate_text(&input.name, "model group name", 120)?;
        if input
            .description
            .as_ref()
            .is_some_and(|text| text.chars().count() > 2000)
        {
            return Err(HarnessError::invalid(
                "model group description exceeds 2000 characters",
            ));
        }
        let generated = random_identifier("mgp");
        let group_id = id.unwrap_or(&generated);
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelGrantsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-group:{group_id}")).await?;
        if id.is_some() {
            group_in(&mut tx, group_id).await?;
        }
        sqlx::query("INSERT INTO control_model_groups(group_id,name,description,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$4) ON CONFLICT(group_id) DO UPDATE SET name=EXCLUDED.name,description=EXCLUDED.description,updated_at_ms=EXCLUDED.updated_at_ms")
            .bind(group_id).bind(input.name.trim()).bind(input.description.as_deref().map(str::trim).filter(|text|!text.is_empty())).bind(number(now_ms)?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.group.save",
            group_id,
            json!({"name":input.name}),
            now_ms,
        )
        .await?;
        let value = group_in(&mut tx, group_id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(value)
    }

    pub async fn delete_model_group(
        &self,
        actor: &ControlUser,
        id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelGrantsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-group:{id}")).await?;
        group_in(&mut tx, id).await?;
        sqlx::query(
            "UPDATE control_model_groups SET deleted_at_ms=$2,updated_at_ms=$2 WHERE group_id=$1",
        )
        .bind(id)
        .bind(number(now_ms)?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        sqlx::query("DELETE FROM control_model_group_members WHERE group_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.group.delete",
            id,
            json!({}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn list_model_group_members(
        &self,
        actor: &ControlUser,
        id: &str,
        query: &PageQuery,
    ) -> Result<ModelGroupMemberPage, HarnessError> {
        self.model_group_accounts(actor, id, query, true).await
    }

    pub async fn list_model_group_candidates(
        &self,
        actor: &ControlUser,
        id: &str,
        query: &PageQuery,
    ) -> Result<ModelGroupMemberPage, HarnessError> {
        self.model_group_accounts(actor, id, query, false).await
    }

    async fn model_group_accounts(
        &self,
        actor: &ControlUser,
        id: &str,
        query: &PageQuery,
        members: bool,
    ) -> Result<ModelGroupMemberPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let action = if members {
            PlatformAction::ModelsRead
        } else {
            PlatformAction::ModelGrantsManage
        };
        let mut tx = self.model_admin_transaction(actor, action).await?;
        group_in(&mut tx, id).await?;
        let rows=sqlx::query("SELECT u.user_id,u.username FROM control_users u WHERE (CAST($2 AS BIGINT)=0 OR EXISTS(SELECT 1 FROM control_model_group_members m WHERE m.group_id=$1 AND m.user_id=u.user_id)) AND (CAST($3 AS TEXT) IS NULL OR LOWER(u.user_id) LIKE $3 ESCAPE '!' OR LOWER(u.username) LIKE $3 ESCAPE '!') AND (CAST($4 AS TEXT) IS NULL OR u.user_id>$4) ORDER BY u.user_id LIMIT $5")
            .bind(id).bind(i64::from(members)).bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut users = rows
            .iter()
            .map(|row| {
                Ok(ControlUser {
                    user_id: UserId::new(
                        row.try_get::<String, _>("user_id")
                            .map_err(database_error)?,
                    ),
                    username: row.try_get("username").map_err(database_error)?,
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        let next_cursor = query.finish(&mut users, |user| user.user_id.to_string());
        tx.commit().await.map_err(database_error)?;
        Ok(ModelGroupMemberPage { users, next_cursor })
    }

    pub async fn set_model_group_member(
        &self,
        actor: &ControlUser,
        id: &str,
        user_id: &UserId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        user_id.validate()?;
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelGrantsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-group:{id}")).await?;
        group_in(&mut tx, id).await?;
        let exists: i64 = sqlx::query_scalar(
            "SELECT CAST(EXISTS(SELECT 1 FROM control_users WHERE user_id=$1) AS INTEGER)",
        )
        .bind(user_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        if exists == 0 {
            return Err(HarnessError::invalid("model group account does not exist"));
        }
        sqlx::query("INSERT INTO control_model_group_members(group_id,user_id,created_at_ms) VALUES($1,$2,$3) ON CONFLICT(group_id,user_id) DO NOTHING").bind(id).bind(user_id.as_str()).bind(number(now_ms)?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.group.member.add",
            id,
            json!({"user_id":user_id}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn remove_model_group_member(
        &self,
        actor: &ControlUser,
        id: &str,
        user_id: &UserId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelGrantsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-group:{id}")).await?;
        group_in(&mut tx, id).await?;
        sqlx::query("DELETE FROM control_model_group_members WHERE group_id=$1 AND user_id=$2")
            .bind(id)
            .bind(user_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.group.member.remove",
            id,
            json!({"user_id":user_id}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }
}

pub(super) async fn group_in(
    tx: &mut Transaction,
    id: &str,
) -> Result<ModelGroupRecord, HarnessError> {
    let row=sqlx::query("SELECT g.*,(SELECT COUNT(*) FROM control_model_group_members m WHERE m.group_id=g.group_id) AS member_count FROM control_model_groups g WHERE group_id=$1 AND deleted_at_ms IS NULL")
        .bind(id).fetch_optional(&mut **tx).await.map_err(database_error)?.ok_or_else(||HarnessError::invalid("model group does not exist"))?;
    group_from_row(&row)
}

fn group_from_row(row: &AnyRow) -> Result<ModelGroupRecord, HarnessError> {
    Ok(ModelGroupRecord {
        group_id: row.try_get("group_id").map_err(database_error)?,
        name: row.try_get("name").map_err(database_error)?,
        description: row.try_get("description").map_err(database_error)?,
        member_count: read_number(row, "member_count")?,
        created_at_ms: read_number(row, "created_at_ms")?,
        updated_at_ms: read_number(row, "updated_at_ms")?,
    })
}
