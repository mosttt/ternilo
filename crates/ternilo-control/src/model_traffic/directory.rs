use super::{
    ControlStore, ControlUser, HarnessError, PlatformAction, Row, UserId, authorize_platform_in,
    database_error,
};
use crate::PageQuery;
use serde::Serialize;
use ternilo_storage::Backend;

#[derive(Serialize)]
pub struct ModelTrafficTarget {
    pub user_id: UserId,
    pub username: String,
    pub name: String,
    pub kind: String,
    pub space_name: Option<String>,
}
#[derive(Serialize)]
pub struct ModelTrafficTargetPage {
    pub accounts: Vec<ModelTrafficTarget>,
    pub next_cursor: Option<String>,
}

impl ControlStore {
    pub async fn model_traffic_targets(
        &self,
        actor: &ControlUser,
        query: &PageQuery,
    ) -> Result<ModelTrafficTargetPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        if let Some(id) = &cursor {
            UserId::new(id).validate()?;
        }
        let mut tx = self.database.begin_read().await?;
        authorize_platform_in(&mut tx, &actor.user_id, PlatformAction::AccountsRead).await?;
        crate::model_store::scope(&mut tx).await?;
        let statement = match ternilo_storage::backend(&tx) {
            Backend::Postgres => "SELECT * FROM ternilo_model_traffic_targets($1,$2,$3)",
            Backend::Sqlite => "SELECT u.user_id,u.username,COALESCE(s.name,u.username) AS name,CASE WHEN s.service_account_id IS NULL THEN 'user' ELSE 'service' END AS kind,t.display_name AS space_name
                FROM control_users u LEFT JOIN control_service_accounts s ON s.service_account_id=u.user_id LEFT JOIN control_tenants t ON t.tenant_id=s.tenant_id
                WHERE u.status<>'removed' AND (CAST($2 AS TEXT) IS NULL OR u.user_id<$2)
                  AND (CAST($1 AS TEXT) IS NULL OR LOWER(u.username) LIKE $1 ESCAPE '!' OR LOWER(COALESCE(s.name,u.username)) LIKE $1 ESCAPE '!' OR LOWER(u.user_id) LIKE $1 ESCAPE '!')
                ORDER BY u.user_id DESC LIMIT $3",
        };
        let rows = sqlx::query(statement)
            .bind(pattern)
            .bind(cursor)
            .bind(i64::from(query.limit) + 1)
            .fetch_all(&mut *tx)
            .await
            .map_err(database_error)?;
        let mut accounts = rows
            .into_iter()
            .map(|row| {
                Ok(ModelTrafficTarget {
                    user_id: UserId::new(
                        row.try_get::<String, _>("user_id")
                            .map_err(database_error)?,
                    ),
                    username: row.try_get("username").map_err(database_error)?,
                    name: row.try_get("name").map_err(database_error)?,
                    kind: row.try_get("kind").map_err(database_error)?,
                    space_name: row.try_get("space_name").map_err(database_error)?,
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        let next_cursor = query.finish(&mut accounts, |value| value.user_id.to_string());
        tx.commit().await.map_err(database_error)?;
        Ok(ModelTrafficTargetPage {
            accounts,
            next_cursor,
        })
    }
}
