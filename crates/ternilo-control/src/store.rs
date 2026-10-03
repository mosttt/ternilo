use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use serde_json::{Value, json};
use sqlx::{AnyPool, Row, any::AnyRow};
use ternilo_protocol::{
    AgentPresetDocument, HarnessError, PluginEntry, TenantId, UserId, WorkspaceId,
};
use ternilo_storage::{Backend, Database};
use ternilo_transport::{ExecutorId, ExecutorScope};
use zeroize::Zeroizing;

use crate::{
    AuditEntry, ControlAction, ControlUser, EnrollmentGrant, ExecutorRecord, NodeCredentialGrant,
    NodePrincipal, OidcPrincipal, ProjectRecord, QuotaReservation, SecretCipher, SecretMetadata,
    SpaceKind, TenantQuota, TenantRole, TenantSummary, WorkspacePlacement, WorkspaceRecord,
    WorkspaceStorage,
    crypto::{EncryptedSecret, chained_hash, hex, random_identifier, random_token, token_hash},
    edge_store::EdgeStore,
    types::require_bounded,
};

mod computer_usage;
pub(crate) use computer_usage::project_events as project_usage_events;
pub use computer_usage::{
    ComputerProviderUsage, ComputerProviderUsagePage, ComputerUsageCount, ComputerUsageGroup,
    ComputerUsageSummary, ComputerUsageTotals,
};
mod projects;
mod usage;

const MAX_EXTENSION_PUBLISHERS_PER_TENANT: i64 = 256;
const MAX_EXTENSION_PACKAGES_PER_TENANT: i64 = 256;
const MAX_EXTENSION_STORAGE_BYTES_PER_TENANT: i64 = 512 * 1024 * 1024;

#[derive(Clone)]
pub struct ControlStore {
    pub(crate) database: Database,
    pub(crate) pool: AnyPool,
    pub(crate) cipher: Arc<SecretCipher>,
}

#[derive(Clone, Copy)]
struct WorkspaceCreateSpec<'a> {
    project_id: &'a str,
    name: &'a str,
    placement: WorkspacePlacement,
    storage: WorkspaceStorage,
    executor_id: Option<&'a ExecutorId>,
    executor_workspace_id: Option<&'a WorkspaceId>,
}

fn validate_workspace_create(
    actor: &ControlUser,
    tenant_id: &TenantId,
    spec: &WorkspaceCreateSpec<'_>,
) -> Result<(), HarnessError> {
    tenant_id.validate()?;
    actor.user_id.validate()?;
    require_bounded(spec.project_id, "workspace project id", 128)?;
    require_bounded(spec.name, "workspace name", 256)?;
    if let Some(executor_id) = spec.executor_id {
        executor_id.validate()?;
    }
    if let Some(workspace_id) = spec.executor_workspace_id {
        workspace_id.validate()?;
    }
    let binding_is_valid = match spec.placement {
        WorkspacePlacement::Cloud => {
            matches!(
                spec.storage,
                WorkspaceStorage::CloudVolume | WorkspaceStorage::GitWorktree
            ) && spec.executor_id.is_none()
                && spec.executor_workspace_id.is_none()
        }
        WorkspacePlacement::LocalNode => {
            spec.storage == WorkspaceStorage::LocalPath
                && spec.executor_id.is_some()
                && spec.executor_workspace_id.is_some()
        }
    };
    if binding_is_valid {
        Ok(())
    } else {
        Err(HarnessError::invalid(
            "workspace placement, storage, and executor binding are inconsistent",
        ))
    }
}

async fn ensure_workspace_create_targets(
    transaction: &mut ternilo_storage::Transaction,
    actor: &ControlUser,
    tenant_id: &TenantId,
    spec: &WorkspaceCreateSpec<'_>,
) -> Result<(), HarnessError> {
    let project_exists = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(EXISTS(
            SELECT 1 FROM control_projects WHERE tenant_id = $1 AND project_id = $2
         ) AS INTEGER)",
    )
    .bind(tenant_id.as_str())
    .bind(spec.project_id)
    .fetch_one(&mut **transaction)
    .await
    .map(|value| value != 0)
    .map_err(database_error)?;
    if !project_exists {
        return Err(HarnessError::invalid("workspace project does not exist"));
    }
    let Some(executor_id) = spec.executor_id else {
        return Ok(());
    };
    let executor = sqlx::query(
        "SELECT owner_user_id, project_id, state
         FROM control_executors
         WHERE tenant_id = $1 AND executor_id = $2",
    )
    .bind(tenant_id.as_str())
    .bind(executor_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("workspace executor does not exist"))?;
    let owner: String = executor.try_get("owner_user_id").map_err(database_error)?;
    let executor_project: Option<String> =
        executor.try_get("project_id").map_err(database_error)?;
    let state: String = executor.try_get("state").map_err(database_error)?;
    if state == "revoked"
        || owner != actor.user_id.as_str()
        || executor_project
            .as_deref()
            .is_some_and(|bound| bound != spec.project_id)
    {
        return Err(HarnessError::policy(
            "workspace executor is revoked, belongs to another user, or is bound to another project",
        ));
    }
    Ok(())
}

impl ControlStore {
    #[must_use]
    pub fn edge_store(&self) -> EdgeStore {
        EdgeStore::from_database(self.database.clone())
    }

    #[must_use]
    pub fn database(&self) -> &Database {
        &self.database
    }

    pub async fn connect(
        database_url: &str,
        migration_database_url: Option<&str>,
        cipher: SecretCipher,
        max_connections: u32,
    ) -> Result<Self, HarnessError> {
        if let Some(url) = migration_database_url {
            let owner = Database::connect(url, 1).await?;
            Self::initialize(&owner).await?;
            owner.close().await;
            let database = Database::connect(database_url, max_connections).await?;
            return Ok(Self {
                pool: database.pool().clone(),
                database,
                cipher: Arc::new(cipher),
            });
        }
        let database = Database::connect(database_url, max_connections).await?;
        Self::from_database(database, cipher).await
    }

    pub async fn from_database(
        database: Database,
        cipher: SecretCipher,
    ) -> Result<Self, HarnessError> {
        Self::initialize(&database).await?;
        Ok(Self {
            pool: database.pool().clone(),
            database,
            cipher: Arc::new(cipher),
        })
    }

    async fn initialize(database: &Database) -> Result<(), HarnessError> {
        let schema = match database.backend() {
            Backend::Sqlite => concat!(
                include_str!("schema/sqlite.sql"),
                include_str!("identity_schema.sql"),
                include_str!("sharing_schema.sql"),
                include_str!("model_schema.sql"),
                "CREATE TRIGGER control_platform_audit_no_update BEFORE UPDATE ON control_platform_audit BEGIN SELECT RAISE(ABORT, 'platform audit is append-only'); END;",
                "CREATE TRIGGER control_platform_audit_no_delete BEFORE DELETE ON control_platform_audit BEGIN SELECT RAISE(ABORT, 'platform audit is append-only'); END;"
            ),
            Backend::Postgres => concat!(
                include_str!("schema/postgres.sql"),
                include_str!("identity_schema.sql"),
                include_str!("sharing_schema.sql"),
                include_str!("model_schema.sql")
            ),
        };
        database
            .initialize(
                "control",
                14,
                schema,
                include_str!("schema/postgres_access.sql"),
            )
            .await?;
        crate::authentication_settings::initialize(database).await?;
        crate::account_email::initialize(database).await?;
        crate::project_sharing::initialize(database).await?;
        crate::resource_ownership::initialize(database).await?;
        crate::identity_session_details::initialize(database).await?;
        crate::oidc_sessions::initialize(database).await?;
        crate::mfa::initialize(database).await?;
        crate::computer_management::initialize(database).await?;
        crate::service_accounts::initialize(database).await?;
        crate::computer_models::initialize(database).await?;
        computer_usage::initialize(database).await?;
        crate::node_account_cleanup::initialize(database).await
    }

    pub async fn health(&self) -> Result<(), HarnessError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
        Ok(())
    }

    /// Re-encrypt every stored tenant-secret version under a new master key.
    ///
    /// This operator operation must use the migration-owner URL because tenant
    /// RLS intentionally hides rows from an unscoped runtime connection. The
    /// table lock prevents control-plane reads or writes from observing a
    /// partially rotated key set, and the transaction rolls the whole operation
    /// back if any ciphertext cannot be authenticated with `current_cipher`.
    pub async fn rotate_secret_master_key(
        database_url: &str,
        current_cipher: &SecretCipher,
        next_cipher: &SecretCipher,
    ) -> Result<u64, HarnessError> {
        if database_url.trim().is_empty() {
            return Err(HarnessError::invalid(
                "migration-owner database URL is required for master-key rotation",
            ));
        }
        let database = Database::connect(database_url, 1).await?;
        let result =
            rotate_secret_master_key_in_database(&database, current_cipher, next_cipher).await;
        database.close().await;
        result
    }

    /// Provision an identity with an explicit username through a trusted internal path.
    /// Browser OIDC registration must use `register_oidc` for admission checks.
    pub async fn upsert_user(
        &self,
        principal: &OidcPrincipal,
        username: &str,
        now_ms: u64,
    ) -> Result<ControlUser, HarnessError> {
        principal.validate()?;
        let username = crate::identity_store::normalize_username(username)?;
        let mut transaction = self.database.begin().await?;
        ternilo_storage::lock(
            &mut transaction,
            &format!("ternilo:oidc:{}:{}", principal.issuer, principal.subject),
        )
        .await?;
        let user = Self::upsert_user_in(
            &mut transaction,
            principal,
            &username,
            crate::AccountStatus::Active,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(user)
    }

    pub(crate) async fn upsert_user_in(
        transaction: &mut ternilo_storage::Transaction,
        principal: &OidcPrincipal,
        username: &str,
        status: crate::AccountStatus,
        now_ms: u64,
    ) -> Result<ControlUser, HarnessError> {
        if let Some(user) = Self::refresh_oidc_user_in(transaction, principal, now_ms).await? {
            return Ok(user);
        }
        let user = ControlUser {
            user_id: UserId::new(random_identifier("usr")),
            username: username.to_owned(),
        };
        crate::identity_store::require_available_username_in(transaction, username).await?;
        let email = principal
            .email
            .as_deref()
            .map(crate::identity_store::normalize_email)
            .transpose()?;
        if let Some(email) = &email {
            crate::identity_store::reserve_email_in(transaction, email).await?;
        }
        let created = sqlx::query(
            "INSERT INTO control_users
                (user_id, issuer, subject, email, username, created_at_ms, last_seen_at_ms, status)
             VALUES ($1, $2, $3, $4, $5, $6, $6, $7)
             ON CONFLICT (username) DO NOTHING",
        )
        .bind(user.user_id.as_str())
        .bind(&principal.issuer)
        .bind(&principal.subject)
        .bind(email)
        .bind(username)
        .bind(to_i64(now_ms, "user timestamp")?)
        .bind(status.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        if created.rows_affected() != 1 {
            return Err(HarnessError::conflict("username is already registered"));
        }
        Self::create_personal_space_in(transaction, &user, now_ms).await?;
        Ok(user)
    }

    pub(crate) async fn refresh_oidc_user_in(
        transaction: &mut ternilo_storage::Transaction,
        principal: &OidcPrincipal,
        now_ms: u64,
    ) -> Result<Option<ControlUser>, HarnessError> {
        let row = sqlx::query("UPDATE control_users SET last_seen_at_ms = $3 WHERE issuer = $1 AND subject = $2 RETURNING user_id, username, status")
            .bind(&principal.issuer).bind(&principal.subject)
            .bind(to_i64(now_ms, "user timestamp")?)
            .fetch_optional(&mut **transaction).await.map_err(database_error)?;
        row.map(|row| {
            let status = crate::AccountStatus::parse(
                &row.try_get::<String, _>("status").map_err(database_error)?,
            )?;
            if matches!(
                status,
                crate::AccountStatus::Banned | crate::AccountStatus::Removed
            ) {
                status.require_active()?;
            }
            Ok(ControlUser {
                user_id: UserId::new(
                    row.try_get::<String, _>("user_id")
                        .map_err(database_error)?,
                ),
                username: row.try_get("username").map_err(database_error)?,
            })
        })
        .transpose()
    }

    pub async fn create_tenant(
        &self,
        actor: &ControlUser,
        slug: &str,
        display_name: &str,
        quota: TenantQuota,
        now_ms: u64,
    ) -> Result<TenantSummary, HarnessError> {
        let mut transaction = self.database.begin().await?;
        let tenant = Self::create_tenant_in(
            &mut transaction,
            actor,
            slug,
            display_name,
            quota,
            SpaceKind::Team,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(tenant)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_tenant_in(
        transaction: &mut ternilo_storage::Transaction,
        actor: &ControlUser,
        slug: &str,
        display_name: &str,
        quota: TenantQuota,
        kind: SpaceKind,
        now_ms: u64,
    ) -> Result<TenantSummary, HarnessError> {
        validate_slug(slug)?;
        require_bounded(display_name, "tenant display name", 256)?;
        quota.validate()?;
        actor.user_id.validate()?;
        let tenant_id = TenantId::new(random_identifier("ten"));
        let project_id = random_identifier("prj");
        let now = to_i64(now_ms, "tenant timestamp")?;
        set_tenant(transaction, &tenant_id).await?;
        sqlx::query(
            "INSERT INTO control_tenants
                (tenant_id, slug, display_name, created_by, created_at_ms, kind)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tenant_id.as_str())
        .bind(slug)
        .bind(display_name)
        .bind(actor.user_id.as_str())
        .bind(now)
        .bind(kind.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO control_memberships (tenant_id, user_id, role, created_at_ms)
             VALUES ($1, $2, 'owner', $3)",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO control_projects
                (tenant_id, project_id, name, created_by, created_at_ms)
             VALUES ($1, $2, 'Default', $3, $4)",
        )
        .bind(tenant_id.as_str())
        .bind(project_id)
        .bind(actor.user_id.as_str())
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        insert_quota(transaction, &tenant_id, &actor.user_id, &quota, now).await?;
        append_audit(
            transaction,
            &tenant_id,
            Some(&actor.user_id),
            "user",
            "tenant.create",
            "tenant",
            tenant_id.as_str(),
            "success",
            json!({ "slug": slug }),
            now_ms,
        )
        .await?;
        Ok(TenantSummary {
            tenant_id,
            kind,
            slug: slug.to_owned(),
            display_name: display_name.to_owned(),
            role: TenantRole::Owner,
        })
    }

    pub async fn list_tenants(
        &self,
        actor: &ControlUser,
    ) -> Result<Vec<TenantSummary>, HarnessError> {
        actor.user_id.validate()?;
        let rows = sqlx::query(
            match self.database.backend() {
                Backend::Postgres => "SELECT tenant_id, slug, display_name, role, kind FROM ternilo_list_user_tenants($1)",
                Backend::Sqlite => "SELECT tenant.tenant_id, tenant.slug, tenant.display_name, membership.role, tenant.kind FROM control_memberships AS membership JOIN control_tenants AS tenant USING (tenant_id) WHERE membership.user_id = $1 ORDER BY tenant.created_at_ms, tenant.tenant_id",
            },
        )
        .bind(actor.user_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(TenantSummary {
                    tenant_id: TenantId::new(
                        row.try_get::<String, _>("tenant_id")
                            .map_err(database_error)?,
                    ),
                    kind: SpaceKind::parse(
                        &row.try_get::<String, _>("kind").map_err(database_error)?,
                    )?,
                    slug: row.try_get("slug").map_err(database_error)?,
                    display_name: row.try_get("display_name").map_err(database_error)?,
                    role: TenantRole::parse(
                        &row.try_get::<String, _>("role").map_err(database_error)?,
                    )?,
                })
            })
            .collect()
    }

    pub async fn authorize(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        action: ControlAction,
    ) -> Result<TenantRole, HarnessError> {
        tenant_id.validate()?;
        actor.user_id.validate()?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let role = require_action(&mut transaction, tenant_id, &actor.user_id, action).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(role)
    }

    pub async fn create_project(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        name: &str,
        now_ms: u64,
    ) -> Result<ProjectRecord, HarnessError> {
        tenant_id.validate()?;
        require_bounded(name, "project name", 256)?;
        let project_id = random_identifier("prj");
        let now = to_i64(now_ms, "project timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::ProjectManage,
        )
        .await?;
        sqlx::query(
            "INSERT INTO control_projects
                (tenant_id, project_id, name, created_by, created_at_ms)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(tenant_id.as_str())
        .bind(&project_id)
        .bind(name)
        .bind(actor.user_id.as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "project.create",
            "project",
            &project_id,
            "success",
            json!({ "name": name }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(ProjectRecord {
            tenant_id: tenant_id.clone(),
            project_id,
            name: name.to_owned(),
            created_at_ms: now_ms,
        })
    }

    pub async fn list_projects(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<ProjectRecord>, HarnessError> {
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::TenantRead,
        )
        .await?;
        let rows = sqlx::query(
            "SELECT project_id, name, created_at_ms FROM control_projects
             WHERE tenant_id = $1 ORDER BY created_at_ms, project_id",
        )
        .bind(tenant_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(ProjectRecord {
                    tenant_id: tenant_id.clone(),
                    project_id: row.try_get("project_id").map_err(database_error)?,
                    name: row.try_get("name").map_err(database_error)?,
                    created_at_ms: from_i64(
                        row.try_get("created_at_ms").map_err(database_error)?,
                        "project timestamp",
                    )?,
                })
            })
            .collect()
    }

    pub async fn create_cloud_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        project_id: &str,
        name: &str,
        now_ms: u64,
    ) -> Result<WorkspaceRecord, HarnessError> {
        self.create_workspace(
            actor,
            tenant_id,
            WorkspaceCreateSpec {
                project_id,
                name,
                placement: WorkspacePlacement::Cloud,
                storage: WorkspaceStorage::CloudVolume,
                executor_id: None,
                executor_workspace_id: None,
            },
            now_ms,
        )
        .await
    }

    pub async fn create_local_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        project_id: &str,
        name: &str,
        executor_binding: (&ExecutorId, &WorkspaceId),
        now_ms: u64,
    ) -> Result<WorkspaceRecord, HarnessError> {
        let (executor_id, executor_workspace_id) = executor_binding;
        self.create_workspace(
            actor,
            tenant_id,
            WorkspaceCreateSpec {
                project_id,
                name,
                placement: WorkspacePlacement::LocalNode,
                storage: WorkspaceStorage::LocalPath,
                executor_id: Some(executor_id),
                executor_workspace_id: Some(executor_workspace_id),
            },
            now_ms,
        )
        .await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep registration, restoration, ownership checks, and audit in one transaction."
    )]
    async fn create_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        spec: WorkspaceCreateSpec<'_>,
        now_ms: u64,
    ) -> Result<WorkspaceRecord, HarnessError> {
        validate_workspace_create(actor, tenant_id, &spec)?;
        let workspace_id = WorkspaceId::new(random_identifier("wsp"));
        let now = to_i64(now_ms, "workspace timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::RunReserve,
        )
        .await?;
        ensure_workspace_create_targets(&mut transaction, actor, tenant_id, &spec).await?;

        if let (Some(executor_id), Some(node_workspace_id)) =
            (spec.executor_id, spec.executor_workspace_id)
        {
            ternilo_storage::lock(
                &mut transaction,
                &format!("node-resources:{tenant_id}:{executor_id}"),
            )
            .await?;
            let existing = sqlx::query(
                "SELECT workspace_id, project_id, owner_user_id FROM control_workspaces
                 WHERE tenant_id = $1 AND executor_id = $2 AND executor_workspace_id = $3",
            )
            .bind(tenant_id.as_str())
            .bind(executor_id.as_str())
            .bind(node_workspace_id.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(workspace_write_error)?;
            if let Some(existing) = existing {
                if existing
                    .try_get::<String, _>("owner_user_id")
                    .map_err(workspace_write_error)?
                    != actor.user_id.as_str()
                    || existing
                        .try_get::<String, _>("project_id")
                        .map_err(workspace_write_error)?
                        != spec.project_id
                {
                    return Err(HarnessError::policy(
                        "workspace is already bound to another owner or project",
                    ));
                }
                let id: String = existing
                    .try_get("workspace_id")
                    .map_err(workspace_write_error)?;
                crate::resource_ownership::require_workspace_name_in(
                    &mut transaction,
                    tenant_id,
                    &actor.user_id,
                    spec.project_id,
                    spec.name,
                    Some(&id),
                )
                .await?;
                let row = sqlx::query(
                    "UPDATE control_workspaces SET name = $3, updated_at_ms = $4, unregistered_at_ms = NULL
                     WHERE tenant_id = $1 AND workspace_id = $2
                     RETURNING tenant_id, workspace_id, project_id, owner_user_id, name,
                               placement, storage, executor_id, executor_workspace_id, created_at_ms, updated_at_ms",
                ).bind(tenant_id.as_str()).bind(&id).bind(spec.name).bind(now)
                    .fetch_one(&mut *transaction).await.map_err(workspace_write_error)?;
                append_audit(
                    &mut transaction,
                    tenant_id,
                    Some(&actor.user_id),
                    "user",
                    "workspace.register",
                    "workspace",
                    &id,
                    "success",
                    json!({"name": spec.name}),
                    now_ms,
                )
                .await?;
                transaction.commit().await.map_err(workspace_write_error)?;
                return workspace_from_row(&row);
            }
        }

        crate::resource_ownership::require_workspace_name_in(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            spec.project_id,
            spec.name,
            None,
        )
        .await?;
        sqlx::query(
            "INSERT INTO control_workspaces
                (tenant_id, workspace_id, project_id, owner_user_id, name,
                 placement, storage, executor_id, executor_workspace_id,
                 created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
        )
        .bind(tenant_id.as_str())
        .bind(workspace_id.as_str())
        .bind(spec.project_id)
        .bind(actor.user_id.as_str())
        .bind(spec.name)
        .bind(spec.placement.as_str())
        .bind(spec.storage.as_str())
        .bind(spec.executor_id.map(ExecutorId::as_str))
        .bind(spec.executor_workspace_id.map(WorkspaceId::as_str))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(workspace_write_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "workspace.create",
            "workspace",
            workspace_id.as_str(),
            "success",
            json!({
                "project_id": spec.project_id,
                "name": spec.name,
                "placement": spec.placement,
                "storage": spec.storage,
                "executor_id": spec.executor_id.map(ExecutorId::as_str),
                "executor_workspace_id": spec.executor_workspace_id.map(WorkspaceId::as_str),
            }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(workspace_write_error)?;

        Ok(WorkspaceRecord {
            tenant_id: tenant_id.clone(),
            workspace_id,
            project_id: spec.project_id.to_owned(),
            owner_user_id: actor.user_id.clone(),
            name: spec.name.to_owned(),
            placement: spec.placement,
            storage: spec.storage,
            executor_id: spec.executor_id.cloned(),
            executor_workspace_id: spec.executor_workspace_id.cloned(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        })
    }

    pub async fn list_workspaces(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<WorkspaceRecord>, HarnessError> {
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let role = require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::TenantRead,
        )
        .await?;
        let may_read_all = matches!(role, TenantRole::Admin | TenantRole::Owner);
        let rows = sqlx::query(
            "SELECT tenant_id, workspace_id, project_id, owner_user_id, name,
                    placement, storage, executor_id, executor_workspace_id,
                    created_at_ms, updated_at_ms
             FROM control_workspaces
             WHERE tenant_id = $1 AND unregistered_at_ms IS NULL
               AND ($2 != 0 OR owner_user_id = $3)
             ORDER BY updated_at_ms DESC, workspace_id",
        )
        .bind(tenant_id.as_str())
        .bind(i64::from(may_read_all))
        .bind(actor.user_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.iter().map(workspace_from_row).collect()
    }

    pub async fn get_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        workspace_id: &WorkspaceId,
    ) -> Result<WorkspaceRecord, HarnessError> {
        workspace_id.validate()?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let role = require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::TenantRead,
        )
        .await?;
        let may_read_all = matches!(role, TenantRole::Admin | TenantRole::Owner);
        let row = sqlx::query(
            "SELECT w.* FROM control_workspaces w LEFT JOIN control_resource_ownership o
               ON o.tenant_id=w.tenant_id AND o.resource_kind='workspace' AND o.resource_id=w.workspace_id
             WHERE w.tenant_id=$1 AND w.workspace_id=$2 AND w.unregistered_at_ms IS NULL
               AND ($3 != 0 OR COALESCE(o.owner_user_id,w.owner_user_id)=$4)",
        )
        .bind(tenant_id.as_str())
        .bind(workspace_id.as_str())
        .bind(i64::from(may_read_all))
        .bind(actor.user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("workspace does not exist"))?;
        transaction.commit().await.map_err(database_error)?;
        workspace_from_row(&row)
    }

    pub async fn set_membership(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        user_id: &UserId,
        role: TenantRole,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        tenant_id.validate()?;
        user_id.validate()?;
        let now = to_i64(now_ms, "membership timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        crate::account_store::require_team_in(&mut transaction, tenant_id).await?;
        let actor_role = require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::MembershipManage,
        )
        .await?;
        ternilo_storage::lock(
            &mut transaction,
            &(format!("{}:memberships", tenant_id.as_str())),
        )
        .await?;
        let current = sqlx::query_scalar::<_, String>(ternilo_storage::for_update(
            &transaction,
            "SELECT role FROM control_memberships
             WHERE tenant_id = $1 AND user_id = $2",
            "SELECT role FROM control_memberships
             WHERE tenant_id = $1 AND user_id = $2 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let user_exists = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(EXISTS(SELECT 1 FROM control_users WHERE user_id = $1) AS INTEGER)",
        )
        .bind(user_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map(|value| value != 0)
        .map_err(database_error)?;
        if !user_exists {
            return Err(HarnessError::invalid("membership user does not exist"));
        }
        if actor_role != TenantRole::Owner
            && (role == TenantRole::Owner || current.as_deref() == Some("owner"))
        {
            return Err(HarnessError::policy(
                "only a tenant owner may grant or change the owner role",
            ));
        }
        if current.as_deref() == Some("owner") && role != TenantRole::Owner {
            let owners = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM control_memberships
                 WHERE tenant_id = $1 AND role = 'owner'",
            )
            .bind(tenant_id.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
            if owners <= 1 {
                return Err(HarnessError::policy(
                    "the last tenant owner cannot be downgraded",
                ));
            }
        }
        sqlx::query(
            "INSERT INTO control_memberships (tenant_id, user_id, role, created_at_ms)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (tenant_id, user_id) DO UPDATE SET role = EXCLUDED.role",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(role.as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "membership.set",
            "user",
            user_id.as_str(),
            "success",
            json!({ "role": role }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn remove_membership(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        user_id: &UserId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        tenant_id.validate()?;
        user_id.validate()?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        crate::account_store::require_team_in(&mut transaction, tenant_id).await?;
        let actor_role = require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::MembershipManage,
        )
        .await?;
        ternilo_storage::lock(
            &mut transaction,
            &(format!("{}:memberships", tenant_id.as_str())),
        )
        .await?;
        let target_role = sqlx::query_scalar::<_, String>(ternilo_storage::for_update(
            &transaction,
            "SELECT role FROM control_memberships
             WHERE tenant_id = $1 AND user_id = $2",
            "SELECT role FROM control_memberships
             WHERE tenant_id = $1 AND user_id = $2 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("membership does not exist"))?;
        if target_role == "owner" {
            if actor_role != TenantRole::Owner {
                return Err(HarnessError::policy(
                    "only a tenant owner may remove an owner",
                ));
            }
            let owners = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM control_memberships
                 WHERE tenant_id = $1 AND role = 'owner'",
            )
            .bind(tenant_id.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
            if owners <= 1 {
                return Err(HarnessError::policy(
                    "the last tenant owner cannot be removed",
                ));
            }
        }
        sqlx::query("DELETE FROM control_memberships WHERE tenant_id = $1 AND user_id = $2")
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "membership.remove",
            "user",
            user_id.as_str(),
            "success",
            Value::Object(serde_json::Map::new()),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    #[allow(clippy::too_many_lines)]
    pub async fn create_enrollment(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        executor_id: ExecutorId,
        ttl: Duration,
        now_ms: u64,
    ) -> Result<EnrollmentGrant, HarnessError> {
        self.create_enrollment_with_action(
            actor,
            tenant_id,
            project_id,
            crate::computer_management::enrollment::EnrollmentIdentity {
                name: executor_id.to_string(),
                executor_id,
                recovery_revision: None,
            },
            ttl,
            now_ms,
            ControlAction::ExecutorManage,
            false,
        )
        .await
    }

    pub async fn create_owned_enrollment(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        executor_id: ExecutorId,
        ttl: Duration,
        now_ms: u64,
    ) -> Result<EnrollmentGrant, HarnessError> {
        self.create_enrollment_with_action(
            actor,
            tenant_id,
            project_id,
            crate::computer_management::enrollment::EnrollmentIdentity {
                name: executor_id.to_string(),
                executor_id,
                recovery_revision: None,
            },
            ttl,
            now_ms,
            ControlAction::RunReserve,
            true,
        )
        .await
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) async fn create_enrollment_with_action(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        identity: crate::computer_management::enrollment::EnrollmentIdentity,
        ttl: Duration,
        now_ms: u64,
        action: ControlAction,
        owned_only: bool,
    ) -> Result<EnrollmentGrant, HarnessError> {
        let executor_id = identity.executor_id;
        let mut computer_name =
            crate::computer_management::names::validate(&identity.name)?.to_owned();
        tenant_id.validate()?;
        executor_id.validate()?;
        if ttl.is_zero() || ttl > Duration::from_hours(1) {
            return Err(HarnessError::invalid(
                "executor enrollment TTL must be between 1 millisecond and 1 hour",
            ));
        }
        if let Some(project_id) = project_id {
            require_bounded(project_id, "project id", 128)?;
        }
        let enrollment_id = random_identifier("enr");
        let token = random_token("ter_e");
        let token_hash = token_hash(&token).to_vec();
        let expires_at_ms = now_ms.saturating_add(duration_ms(ttl)?);
        let now = to_i64(now_ms, "enrollment timestamp")?;
        let expiry = to_i64(expires_at_ms, "enrollment expiry")?;
        let mut transaction = self.database.begin().await?;
        ternilo_storage::lock(
            &mut transaction,
            &format!("ternilo:account-role:{}", actor.user_id),
        )
        .await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(&mut transaction, tenant_id, &actor.user_id, action).await?;
        ternilo_storage::lock(
            &mut transaction,
            &format!("computer-management:{tenant_id}:{executor_id}"),
        )
        .await?;
        let previous = sqlx::query("SELECT owner_user_id,project_id,state FROM control_executors WHERE tenant_id=$1 AND executor_id=$2")
            .bind(tenant_id.as_str()).bind(executor_id.as_str()).fetch_optional(&mut *transaction).await.map_err(database_error)?;
        let mut owner = actor.user_id.to_string();
        let mut project_id = project_id.map(str::to_owned);
        let mut recovery = None;
        if let Some(revision) = identity.recovery_revision {
            let previous = previous
                .as_ref()
                .ok_or_else(|| HarnessError::invalid("computer does not exist"))?;
            owner = previous.try_get("owner_user_id").map_err(database_error)?;
            if owned_only && owner != actor.user_id.as_str() {
                return Err(HarnessError::policy("computer belongs to another account"));
            }
            if previous
                .try_get::<String, _>("state")
                .map_err(database_error)?
                != "revoked"
            {
                return Err(HarnessError::conflict(
                    "only revoked or removed computers need recovery; resume suspended computers with their original credential",
                ));
            }
            project_id = previous.try_get("project_id").map_err(database_error)?;
            let mut management = crate::computer_management::management_in(
                &mut transaction,
                tenant_id,
                &executor_id,
            )
            .await?;
            if management.revision != revision {
                return Err(HarnessError::conflict(
                    "computer settings changed; refresh before recovery",
                ));
            }
            management.revision = revision
                .checked_add(1)
                .ok_or_else(|| HarnessError::conflict("computer revision exhausted"))?;
            management.name.clone_from(&computer_name);
            recovery = Some(management);
        } else if let Some(previous) = &previous {
            if previous
                .try_get::<String, _>("owner_user_id")
                .map_err(database_error)?
                != owner
            {
                return Err(HarnessError::policy(
                    "computer identity cannot be assigned to another account",
                ));
            }
            computer_name = crate::computer_management::management_in(
                &mut transaction,
                tenant_id,
                &executor_id,
            )
            .await?
            .name;
        }
        crate::account_store::require_active_account_in(&mut transaction, &UserId::new(&owner))
            .await?;
        if let Some(project_id) = &project_id {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT CAST(EXISTS(
                    SELECT 1 FROM control_projects
                    WHERE tenant_id = $1 AND project_id = $2
                 ) AS INTEGER)",
            )
            .bind(tenant_id.as_str())
            .bind(project_id)
            .fetch_one(&mut *transaction)
            .await
            .map(|value| value != 0)
            .map_err(database_error)?;
            if !exists {
                return Err(HarnessError::invalid("enrollment project does not exist"));
            }
        }
        // The quota row serializes enrollment creation for this tenant. Active,
        // unconsumed grants count too, so concurrent tokens cannot overbook nodes.
        let max_nodes = sqlx::query_scalar::<_, i32>(ternilo_storage::for_update(
            &transaction,
            "SELECT max_nodes FROM control_quotas WHERE tenant_id = $1",
            "SELECT max_nodes FROM control_quotas WHERE tenant_id = $1 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if owned_only {
            let owned_by_another_user = sqlx::query_scalar::<_, i64>(
                "SELECT CAST(EXISTS(
                     SELECT 1 FROM control_executors
                     WHERE tenant_id = $1 AND executor_id = $2 AND owner_user_id != $3
                     UNION ALL
                     SELECT 1 FROM control_executor_enrollments
                     WHERE tenant_id = $1 AND executor_id = $2 AND created_by != $3
                       AND consumed_at_ms IS NULL AND expires_at_ms > $4
                 ) AS INTEGER)",
            )
            .bind(tenant_id.as_str())
            .bind(executor_id.as_str())
            .bind(actor.user_id.as_str())
            .bind(now)
            .fetch_one(&mut *transaction)
            .await
            .map(|value| value != 0)
            .map_err(database_error)?;
            if owned_by_another_user {
                return Err(HarnessError::policy("executor id belongs to another user"));
            }
        }
        let executor_is_active = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(EXISTS(
                 SELECT 1 FROM control_executors
                 WHERE tenant_id = $1 AND executor_id = $2 AND state != 'revoked'
             ) AS INTEGER)",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map(|value| value != 0)
        .map_err(database_error)?;
        if executor_is_active {
            return Err(HarnessError::policy(
                "executor must be revoked before it can be enrolled again",
            ));
        }
        let reserved_nodes = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM (
                 SELECT executor_id FROM control_executors
                 WHERE tenant_id = $1 AND state != 'revoked'
                 UNION
                 SELECT executor_id FROM control_executor_enrollments
                 WHERE tenant_id = $1 AND consumed_at_ms IS NULL AND expires_at_ms > $2
             ) AS reserved_nodes",
        )
        .bind(tenant_id.as_str())
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let executor_is_reserved = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(EXISTS(
                 SELECT 1 FROM control_executors
                 WHERE tenant_id = $1 AND executor_id = $2 AND state != 'revoked'
                 UNION ALL
                 SELECT 1 FROM control_executor_enrollments
                 WHERE tenant_id = $1 AND executor_id = $2
                   AND consumed_at_ms IS NULL AND expires_at_ms > $3
             ) AS INTEGER)",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map(|value| value != 0)
        .map_err(database_error)?;
        if !executor_is_reserved && reserved_nodes >= i64::from(max_nodes) {
            return Err(HarnessError::policy("tenant node quota is exhausted"));
        }
        let name_expiry = if previous.is_none()
            || recovery
                .as_ref()
                .is_some_and(|management| management.removed_at_ms.is_some())
        {
            Some(expires_at_ms)
        } else {
            None
        };
        crate::computer_management::names::reserve_in(
            &mut transaction,
            tenant_id,
            &executor_id,
            &owner,
            &computer_name,
            name_expiry,
            now_ms,
        )
        .await?;
        if let Some(management) = &recovery {
            sqlx::query("UPDATE control_executor_enrollments SET consumed_at_ms=$3 WHERE tenant_id=$1 AND executor_id=$2 AND consumed_at_ms IS NULL")
                .bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(now).execute(&mut *transaction).await.map_err(database_error)?;
            crate::computer_management::persist_in(
                &mut transaction,
                tenant_id,
                &executor_id,
                management,
            )
            .await?;
        }
        sqlx::query(
            "INSERT INTO control_executor_enrollments
                (enrollment_id, tenant_id, project_id, executor_id, token_hash,
                 expires_at_ms, created_by, created_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&enrollment_id)
        .bind(tenant_id.as_str())
        .bind(project_id.as_deref())
        .bind(executor_id.as_str())
        .bind(token_hash)
        .bind(expiry)
        .bind(&owner)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "executor.enrollment.create",
            "executor",
            executor_id.as_str(),
            "success",
            json!({ "enrollment_id": enrollment_id, "expires_at_ms": expires_at_ms }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(EnrollmentGrant {
            enrollment_id,
            tenant_id: tenant_id.clone(),
            executor_id,
            name: computer_name,
            expires_at_ms,
            token,
        })
    }

    pub async fn consume_enrollment(
        &self,
        enrollment_token: &str,
        now_ms: u64,
    ) -> Result<NodeCredentialGrant, HarnessError> {
        if enrollment_token.len() > 256 {
            return Err(HarnessError::invalid("invalid enrollment token"));
        }
        let credential_id = random_identifier("ncr");
        let credential_token = random_token("ter_n");
        let mut transaction = self.database.begin().await?;
        let hash = token_hash(enrollment_token).to_vec();
        let tenant = token_tenant(&mut transaction, &hash, true).await?;
        set_tenant(&mut transaction, &tenant).await?;
        let owner: String = sqlx::query_scalar("SELECT created_by FROM control_executor_enrollments WHERE tenant_id = $1 AND token_hash = $2")
            .bind(tenant.as_str()).bind(&hash).fetch_one(&mut *transaction).await.map_err(database_error)?;
        let owner = UserId::new(owner);
        ternilo_storage::lock(&mut transaction, &format!("ternilo:account-role:{owner}")).await?;
        crate::account_store::require_active_account_in(&mut transaction, &owner).await?;
        let enrolled_executor:String=sqlx::query_scalar("SELECT executor_id FROM control_executor_enrollments WHERE tenant_id=$1 AND token_hash=$2")
            .bind(tenant.as_str()).bind(&hash).fetch_one(&mut *transaction).await.map_err(database_error)?;
        ternilo_storage::lock(
            &mut transaction,
            &format!("computer-management:{tenant}:{enrolled_executor}"),
        )
        .await?;
        let row = sqlx::query(ternilo_storage::for_update(&transaction,
            "SELECT enrollment_id, tenant_id, created_by AS user_id, project_id, executor_id FROM control_executor_enrollments WHERE tenant_id = $1 AND token_hash = $2 AND consumed_at_ms IS NULL AND expires_at_ms > $3",
            "SELECT enrollment_id, tenant_id, created_by AS user_id, project_id, executor_id FROM control_executor_enrollments WHERE tenant_id = $1 AND token_hash = $2 AND consumed_at_ms IS NULL AND expires_at_ms > $3 FOR UPDATE",
        ))
        .bind(tenant.as_str())
        .bind(hash)
        .bind(to_i64(now_ms, "enrollment consumption timestamp")?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("enrollment token is invalid, expired, or consumed"))?;
        let tenant_id = TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        );
        let user_id = UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        );
        let executor_id = ExecutorId::new(
            row.try_get::<String, _>("executor_id")
                .map_err(database_error)?,
        );
        let project_id: Option<String> = row.try_get("project_id").map_err(database_error)?;
        set_tenant(&mut transaction, &tenant_id).await?;
        sqlx::query(
            "UPDATE control_executor_enrollments SET consumed_at_ms = $2 WHERE enrollment_id = $1",
        )
        .bind(
            row.try_get::<String, _>("enrollment_id")
                .map_err(database_error)?,
        )
        .bind(to_i64(now_ms, "enrollment consumption timestamp")?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query("INSERT INTO control_executors (tenant_id, executor_id, project_id, owner_user_id, state, enrolled_at_ms) VALUES ($1, $2, $3, $4, 'enrolled', $5) ON CONFLICT (tenant_id, executor_id) DO UPDATE SET project_id = EXCLUDED.project_id, owner_user_id = EXCLUDED.owner_user_id, state = 'enrolled', enrolled_at_ms = EXCLUDED.enrolled_at_ms")
            .bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(&project_id).bind(user_id.as_str())
            .bind(to_i64(now_ms, "executor enrollment timestamp")?)
            .execute(&mut *transaction).await.map_err(database_error)?;
        let name = crate::computer_management::names::activate_in(
            &mut transaction,
            &tenant_id,
            &executor_id,
        )
        .await?;
        sqlx::query("INSERT INTO control_computer_management(tenant_id,executor_id,display_name) VALUES($1,$2,$3) ON CONFLICT(tenant_id,executor_id) DO UPDATE SET display_name=EXCLUDED.display_name,suspended_at_ms=NULL,removed_at_ms=NULL,revision=control_computer_management.revision+1")
            .bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(name).execute(&mut *transaction).await.map_err(database_error)?;
        sqlx::query("INSERT INTO control_node_credentials (credential_id, tenant_id, executor_id, token_hash, issued_at_ms) VALUES ($1, $2, $3, $4, $5)")
            .bind(&credential_id).bind(tenant_id.as_str()).bind(executor_id.as_str())
            .bind(token_hash(&credential_token).to_vec()).bind(to_i64(now_ms, "credential issue timestamp")?)
            .execute(&mut *transaction).await.map_err(database_error)?;
        sqlx::query("INSERT INTO control_node_storage_bindings(tenant_id,credential_id,storage_instance_id,created_at_ms) SELECT $1,$2,b.storage_instance_id,$4 FROM control_node_credentials c JOIN control_node_storage_bindings b ON b.tenant_id=c.tenant_id AND b.credential_id=c.credential_id WHERE c.tenant_id=$1 AND c.executor_id=$3 AND c.credential_id<>$2 ORDER BY c.issued_at_ms DESC LIMIT 1")
            .bind(tenant_id.as_str()).bind(&credential_id).bind(executor_id.as_str()).bind(to_i64(now_ms,"credential issue timestamp")?)
            .execute(&mut *transaction).await.map_err(database_error)?;
        append_audit(
            &mut transaction,
            &tenant_id,
            Some(&user_id),
            "node",
            "executor.enrollment.consume",
            "executor",
            executor_id.as_str(),
            "success",
            json!({ "credential_id": credential_id }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(NodeCredentialGrant {
            credential_id,
            scope: ExecutorScope { tenant_id, user_id },
            executor_id,
            project_id,
            token: credential_token,
        })
    }

    pub async fn authenticate_node(
        &self,
        credential_token: &str,
        now_ms: u64,
    ) -> Result<NodePrincipal, HarnessError> {
        if credential_token.is_empty() || credential_token.len() > 256 {
            return Err(HarnessError::policy("invalid node credential"));
        }
        let mut transaction = self.database.begin().await?;
        let hash = token_hash(credential_token).to_vec();
        let tenant = token_tenant(&mut transaction, &hash, false).await?;
        set_tenant(&mut transaction, &tenant).await?;
        let row = sqlx::query(ternilo_storage::for_update(&transaction,
            "SELECT credential.tenant_id, executor.owner_user_id AS user_id, executor.project_id, credential.executor_id, credential.credential_id FROM control_node_credentials AS credential JOIN control_executors AS executor ON executor.tenant_id = credential.tenant_id AND executor.executor_id = credential.executor_id LEFT JOIN control_computer_management m ON m.tenant_id=executor.tenant_id AND m.executor_id=executor.executor_id WHERE credential.tenant_id = $1 AND credential.token_hash = $2 AND credential.revoked_at_ms IS NULL AND executor.state != 'revoked' AND m.suspended_at_ms IS NULL AND m.removed_at_ms IS NULL",
            "SELECT credential.tenant_id, executor.owner_user_id AS user_id, executor.project_id, credential.executor_id, credential.credential_id FROM control_node_credentials AS credential JOIN control_executors AS executor ON executor.tenant_id = credential.tenant_id AND executor.executor_id = credential.executor_id LEFT JOIN control_computer_management m ON m.tenant_id=executor.tenant_id AND m.executor_id=executor.executor_id WHERE credential.tenant_id = $1 AND credential.token_hash = $2 AND credential.revoked_at_ms IS NULL AND executor.state != 'revoked' AND m.suspended_at_ms IS NULL AND m.removed_at_ms IS NULL FOR UPDATE OF executor",
        ))
        .bind(tenant.as_str()).bind(hash)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("node credential is invalid or revoked"))?;
        let principal = NodePrincipal {
            credential_id: row.try_get("credential_id").map_err(database_error)?,
            scope: ExecutorScope {
                tenant_id: TenantId::new(
                    row.try_get::<String, _>("tenant_id")
                        .map_err(database_error)?,
                ),
                user_id: UserId::new(
                    row.try_get::<String, _>("user_id")
                        .map_err(database_error)?,
                ),
            },
            executor_id: ExecutorId::new(
                row.try_get::<String, _>("executor_id")
                    .map_err(database_error)?,
            ),
            project_id: row.try_get("project_id").map_err(database_error)?,
        };
        crate::account_store::require_active_account_in(&mut transaction, &principal.scope.user_id)
            .await?;
        set_tenant(&mut transaction, &principal.scope.tenant_id).await?;
        // Lock the executor first, then acquire the original credential row through this update.
        let credential_changed = sqlx::query(
            "UPDATE control_node_credentials SET last_used_at_ms = $2 WHERE credential_id = $1 AND revoked_at_ms IS NULL",
        )
        .bind(&principal.credential_id)
        .bind(to_i64(now_ms, "node authentication timestamp")?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?.rows_affected();
        if credential_changed != 1 {
            return Err(HarnessError::policy(
                "node credential is invalid or revoked",
            ));
        }
        let changed = sqlx::query(
            "UPDATE control_executors SET state = 'active', last_seen_at_ms = $3
             WHERE tenant_id = $1 AND executor_id = $2 AND state != 'revoked'",
        )
        .bind(principal.scope.tenant_id.as_str())
        .bind(principal.executor_id.as_str())
        .bind(to_i64(now_ms, "executor last-seen timestamp")?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::policy("node executor has been revoked"));
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(principal)
    }

    pub async fn list_executors(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<ExecutorRecord>, HarnessError> {
        self.list_computers(actor, tenant_id, false, false).await
    }

    pub async fn list_computers(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        owned_only: bool,
        include_removed: bool,
    ) -> Result<Vec<ExecutorRecord>, HarnessError> {
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            if owned_only {
                ControlAction::ExecutorRead
            } else {
                ControlAction::ExecutorManage
            },
        )
        .await?;
        let rows = sqlx::query(concat!(
            include_str!("computer_management/executor_select.sql"),
            " WHERE e.tenant_id=$1 AND (NOT $2 OR e.owner_user_id=$3) AND ($4 OR m.removed_at_ms IS NULL) ORDER BY COALESCE(m.display_name,e.executor_id),e.executor_id"
        ))
        .bind(tenant_id.as_str())
        .bind(owned_only)
        .bind(actor.user_id.as_str())
        .bind(include_removed)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.iter().map(executor_record).collect()
    }

    pub async fn list_owned_executors(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<ExecutorRecord>, HarnessError> {
        self.list_computers(actor, tenant_id, true, false).await
    }

    pub async fn owned_executor(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
    ) -> Result<ExecutorRecord, HarnessError> {
        executor_id.validate()?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::ExecutorRead,
        )
        .await?;
        let row = sqlx::query(concat!(include_str!("computer_management/executor_select.sql")," WHERE e.tenant_id=$1 AND e.owner_user_id=$2 AND e.executor_id=$3 AND m.removed_at_ms IS NULL"))
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(executor_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("owned Node executor does not exist"))?;
        transaction.commit().await.map_err(database_error)?;
        executor_record(&row)
    }

    pub async fn revoke_executor(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        self.revoke_executor_with_scope(
            actor,
            tenant_id,
            executor_id,
            now_ms,
            ControlAction::ExecutorManage,
            false,
        )
        .await
    }

    pub async fn revoke_owned_executor(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        self.revoke_executor_with_scope(
            actor,
            tenant_id,
            executor_id,
            now_ms,
            ControlAction::RunReserve,
            true,
        )
        .await
    }

    async fn revoke_executor_with_scope(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        now_ms: u64,
        action: ControlAction,
        owned_only: bool,
    ) -> Result<(), HarnessError> {
        tenant_id.validate()?;
        executor_id.validate()?;
        let now = to_i64(now_ms, "executor revocation timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(&mut transaction, tenant_id, &actor.user_id, action).await?;
        ternilo_storage::lock(
            &mut transaction,
            &format!("computer-management:{tenant_id}:{executor_id}"),
        )
        .await?;
        // Match account revocation and enrollment consumption: enrollment, executor, credential.
        sqlx::query(
            "UPDATE control_executor_enrollments SET consumed_at_ms = $3
             WHERE tenant_id = $1 AND executor_id = $2 AND consumed_at_ms IS NULL
               AND EXISTS (SELECT 1 FROM control_executors WHERE tenant_id = $1 AND executor_id = $2
                   AND state != 'revoked' AND (NOT $4 OR owner_user_id = $5))",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(now)
        .bind(owned_only)
        .bind(actor.user_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let changed = sqlx::query(
            "UPDATE control_executors SET state = 'revoked'
             WHERE tenant_id = $1 AND executor_id = $2 AND state != 'revoked'
               AND (NOT $3 OR owner_user_id = $4)",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(owned_only)
        .bind(actor.user_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid(if owned_only {
                "owned active executor does not exist"
            } else {
                "active executor does not exist"
            }));
        }
        sqlx::query(
            "UPDATE control_node_credentials SET revoked_at_ms = $3
             WHERE tenant_id = $1 AND executor_id = $2 AND revoked_at_ms IS NULL",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;

        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "executor.revoke",
            "executor",
            executor_id.as_str(),
            "success",
            Value::Object(serde_json::Map::new()),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn get_quota(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<TenantQuota, HarnessError> {
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::TenantRead,
        )
        .await?;
        let row = sqlx::query(
            "SELECT max_nodes, max_concurrent_runs, monthly_model_tokens, max_secrets
             FROM control_quotas WHERE tenant_id = $1",
        )
        .bind(tenant_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        quota_from_row(&row)
    }
    pub async fn update_quota(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        quota: TenantQuota,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        quota.validate()?;
        let now = to_i64(now_ms, "quota timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::QuotaManage,
        )
        .await?;
        insert_quota(&mut transaction, tenant_id, &actor.user_id, &quota, now).await?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "quota.update",
            "tenant",
            tenant_id.as_str(),
            "success",
            serde_json::to_value(&quota).map_err(json_error)?,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn reserve_quota(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        run_id: Option<&str>,
        model_tokens: u64,
        ttl: Duration,
        now_ms: u64,
    ) -> Result<QuotaReservation, HarnessError> {
        let mut transaction = self.database.begin().await?;
        let reservation = Self::reserve_quota_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            run_id,
            model_tokens,
            ttl,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(reservation)
    }

    pub async fn reserve_quota_in(
        transaction: &mut ternilo_storage::Transaction,
        actor_id: &UserId,
        tenant_id: &TenantId,
        run_id: Option<&str>,
        model_tokens: u64,
        ttl: Duration,
        now_ms: u64,
    ) -> Result<QuotaReservation, HarnessError> {
        Self::reserve_quota_for_owner_in(
            transaction,
            actor_id,
            actor_id,
            tenant_id,
            run_id,
            model_tokens,
            ttl,
            now_ms,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Reserve quota, preserve the resource owner and audit the actual actor in one transaction."
    )]
    pub async fn reserve_quota_for_owner_in(
        transaction: &mut ternilo_storage::Transaction,
        actor_id: &UserId,
        owner_id: &UserId,
        tenant_id: &TenantId,
        run_id: Option<&str>,
        model_tokens: u64,
        ttl: Duration,
        now_ms: u64,
    ) -> Result<QuotaReservation, HarnessError> {
        owner_id.validate()?;
        if model_tokens == 0 || ttl.is_zero() || ttl > Duration::from_hours(24) {
            return Err(HarnessError::invalid(
                "quota reservation tokens and TTL must be positive; TTL may not exceed 24 hours",
            ));
        }
        if let Some(run_id) = run_id {
            require_bounded(run_id, "run id", 128)?;
        }
        let reservation_id = random_identifier("qrs");
        let expires_at_ms = now_ms.saturating_add(duration_ms(ttl)?);
        let now = to_i64(now_ms, "quota reservation timestamp")?;
        let expiry = to_i64(expires_at_ms, "quota reservation expiry")?;
        let tokens = to_i64(model_tokens, "quota reservation tokens")?;
        set_tenant(transaction, tenant_id).await?;
        crate::model_store::scope(transaction).await?;
        crate::model_store::lock_budget(transaction, tenant_id).await?;
        require_action(transaction, tenant_id, actor_id, ControlAction::RunReserve).await?;
        sqlx::query(
            "UPDATE control_quota_reservations SET state = 'expired'
             WHERE tenant_id = $1 AND state = 'active' AND expires_at_ms <= $2",
        )
        .bind(tenant_id.as_str())
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        let quota = sqlx::query(ternilo_storage::for_update(
            transaction,
            "SELECT max_nodes, max_concurrent_runs, monthly_model_tokens, max_secrets
             FROM control_quotas WHERE tenant_id = $1",
            "SELECT max_nodes, max_concurrent_runs, monthly_model_tokens, max_secrets
             FROM control_quotas WHERE tenant_id = $1 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
        let quota = quota_from_row(&quota)?;
        let period = usage::quota_period(now_ms)?;
        crate::model_store::refresh_period_in(transaction, tenant_id, &period).await?;
        let budget_used = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(COALESCE((SELECT used_model_tokens+unknown_model_tokens FROM control_quota_usage WHERE tenant_id=$1 AND period_start=$2),0)+COALESCE(SUM(CASE WHEN reserved_model_tokens>COALESCE(committed_model_tokens,0)+unknown_model_tokens THEN reserved_model_tokens-COALESCE(committed_model_tokens,0)-unknown_model_tokens ELSE 0 END),0) AS BIGINT) FROM control_quota_reservations WHERE tenant_id=$1 AND period_start=$2 AND state='active' AND expires_at_ms>$3",
        ).bind(tenant_id.as_str()).bind(&period).bind(now).fetch_one(&mut **transaction).await.map_err(database_error)?;
        let projected = budget_used
            .checked_add(tokens)
            .ok_or_else(|| HarnessError::execution("tenant token accounting overflow"))?;
        if projected > to_i64(quota.monthly_model_tokens, "monthly token quota")? {
            return Err(HarnessError::policy(
                "tenant monthly model-token quota is exhausted",
            ));
        }
        sqlx::query(
            "INSERT INTO control_quota_reservations
                (tenant_id, reservation_id, user_id, run_id, reserved_model_tokens,
                 state, created_at_ms, expires_at_ms, period_start)
             VALUES ($1, $2, $3, $4, $5, 'active', $6, $7, $8)",
        )
        .bind(tenant_id.as_str())
        .bind(&reservation_id)
        .bind(owner_id.as_str())
        .bind(run_id)
        .bind(tokens)
        .bind(now)
        .bind(expiry)
        .bind(&period)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        append_audit(
            transaction,
            tenant_id,
            Some(actor_id),
            "user",
            "quota.reserve",
            "quota_reservation",
            &reservation_id,
            "success",
            json!({ "model_tokens": model_tokens, "run_id": run_id, "owner_user_id": owner_id }),
            now_ms,
        )
        .await?;
        Ok(QuotaReservation {
            reservation_id,
            tenant_id: tenant_id.clone(),
            user_id: owner_id.clone(),
            run_id: run_id.map(str::to_owned),
            reserved_model_tokens: model_tokens,
            expires_at_ms,
        })
    }

    pub async fn release_quota_reservation(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        reservation_id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.begin().await?;
        Self::release_quota_in(&mut tx, actor, tenant_id, reservation_id, false, now_ms).await?;
        tx.commit().await.map_err(database_error)
    }

    /// The execution store must first verify in this transaction that no Run owns this reservation.
    pub async fn release_unallocated_workload_quota_in(
        transaction: &mut ternilo_storage::Transaction,
        actor: &ControlUser,
        tenant_id: &TenantId,
        reservation_id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        Self::release_quota_in(transaction, actor, tenant_id, reservation_id, true, now_ms).await
    }

    async fn release_quota_in(
        transaction: &mut ternilo_storage::Transaction,
        actor: &ControlUser,
        tenant_id: &TenantId,
        reservation_id: &str,
        allow_unallocated_run: bool,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        require_bounded(reservation_id, "quota reservation id", 128)?;
        set_tenant(transaction, tenant_id).await?;
        require_action(
            transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::RunReserve,
        )
        .await?;
        crate::model_store::scope(transaction).await?;
        crate::model_store::lock_budget(transaction, tenant_id).await?;
        let attached = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM control_quota_reservations q WHERE q.tenant_id=$1 AND q.reservation_id=$2 AND (($3=0 AND q.run_id IS NOT NULL) OR EXISTS(SELECT 1 FROM control_model_requests r WHERE r.tenant_id=q.tenant_id AND r.execution_reservation_id=q.reservation_id))")
            .bind(tenant_id.as_str()).bind(reservation_id).bind(i64::from(allow_unallocated_run)).fetch_one(&mut **transaction).await.map_err(database_error)?;
        if attached != 0 {
            return Err(HarnessError::policy(
                "workload reservations must be released through execution cancellation",
            ));
        }
        let changed = sqlx::query(
            "UPDATE control_quota_reservations SET state = 'released'
             WHERE tenant_id = $1 AND reservation_id = $2
               AND state = 'active' AND user_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(reservation_id)
        .bind(actor.user_id.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::policy(
                "active quota reservation is not owned by this user",
            ));
        }
        append_audit(
            transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "quota.release",
            "quota_reservation",
            reservation_id,
            "success",
            Value::Object(serde_json::Map::new()),
            now_ms,
        )
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub async fn put_secret(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        name: &str,
        plaintext: &[u8],
        now_ms: u64,
    ) -> Result<SecretMetadata, HarnessError> {
        validate_secret_name(name)?;
        if let Some(project_id) = project_id {
            require_bounded(project_id, "secret project id", 128)?;
        }
        if plaintext.is_empty() || plaintext.len() > 64 * 1024 {
            return Err(HarnessError::invalid(
                "secret value must contain 1 to 65536 bytes",
            ));
        }
        let now = to_i64(now_ms, "secret timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::SecretManage,
        )
        .await?;
        if let Some(project_id) = project_id {
            let project_exists = sqlx::query_scalar::<_, i64>(
                "SELECT CAST(EXISTS(
                     SELECT 1 FROM control_projects
                     WHERE tenant_id = $1 AND project_id = $2
                 ) AS INTEGER)",
            )
            .bind(tenant_id.as_str())
            .bind(project_id)
            .fetch_one(&mut *transaction)
            .await
            .map(|value| value != 0)
            .map_err(database_error)?;
            if !project_exists {
                return Err(HarnessError::invalid("secret project does not exist"));
            }
        }
        ternilo_storage::lock(
            &mut transaction,
            &(format!(
                "{}:{}:{name}",
                tenant_id.as_str(),
                project_id.unwrap_or("<tenant>")
            )),
        )
        .await?;
        // The quota row serializes creation across different secret names/scopes.
        let max_secrets = sqlx::query_scalar::<_, i32>(ternilo_storage::for_update(
            &transaction,
            "SELECT max_secrets FROM control_quotas WHERE tenant_id = $1",
            "SELECT max_secrets FROM control_quotas WHERE tenant_id = $1 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let current_secret_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM control_secret_heads WHERE tenant_id = $1",
        )
        .bind(tenant_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(EXISTS(
                SELECT 1 FROM control_secret_heads
                WHERE tenant_id = $1 AND project_id IS NOT DISTINCT FROM $2 AND name = $3
             ) AS INTEGER)",
        )
        .bind(tenant_id.as_str())
        .bind(project_id)
        .bind(name)
        .fetch_one(&mut *transaction)
        .await
        .map(|value| value != 0)
        .map_err(database_error)?;
        if !exists && current_secret_count >= i64::from(max_secrets) {
            return Err(HarnessError::policy("tenant secret quota is exhausted"));
        }
        let current_version = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(version) FROM control_secrets
             WHERE tenant_id = $1 AND project_id IS NOT DISTINCT FROM $2 AND name = $3",
        )
        .bind(tenant_id.as_str())
        .bind(project_id)
        .bind(name)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?
        .unwrap_or(0);
        let version = current_version
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("secret version overflow"))?;
        let version_u64 = from_i64(version, "secret version")?;
        let encrypted =
            self.cipher
                .encrypt(tenant_id.as_str(), project_id, name, version_u64, plaintext)?;
        let secret_id = random_identifier("sec");
        sqlx::query(
            "INSERT INTO control_secrets
                (secret_id, tenant_id, project_id, name, version, nonce, ciphertext,
                 created_by, created_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&secret_id)
        .bind(tenant_id.as_str())
        .bind(project_id)
        .bind(name)
        .bind(version)
        .bind(encrypted.nonce.to_vec())
        .bind(encrypted.ciphertext)
        .bind(actor.user_id.as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO control_secret_heads (tenant_id, project_id, name, secret_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (tenant_id, (COALESCE(project_id, '')), name)
             DO UPDATE SET secret_id = EXCLUDED.secret_id",
        )
        .bind(tenant_id.as_str())
        .bind(project_id)
        .bind(name)
        .bind(&secret_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "secret.rotate",
            "secret",
            name,
            "success",
            json!({
                "version": version_u64,
                "secret_id": secret_id,
                "project_id": project_id
            }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(SecretMetadata {
            secret_id,
            project_id: project_id.map(str::to_owned),
            name: name.to_owned(),
            version: version_u64,
            created_at_ms: now_ms,
        })
    }

    pub async fn list_secrets(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<SecretMetadata>, HarnessError> {
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::SecretManage,
        )
        .await?;
        let rows = sqlx::query(
            "SELECT secret.secret_id, secret.project_id, secret.name, secret.version,
                    secret.created_at_ms
             FROM control_secret_heads AS head
             JOIN control_secrets AS secret ON secret.secret_id = head.secret_id
             WHERE head.tenant_id = $1 ORDER BY secret.name",
        )
        .bind(tenant_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.iter().map(secret_metadata_from_row).collect()
    }

    pub async fn delete_secret(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        name: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_secret_name(name)?;
        if let Some(project_id) = project_id {
            require_bounded(project_id, "secret project id", 128)?;
        }
        let now = to_i64(now_ms, "secret deletion timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::SecretManage,
        )
        .await?;
        let secret_id = sqlx::query_scalar::<_, String>(
            "DELETE FROM control_secret_heads
             WHERE tenant_id = $1 AND project_id IS NOT DISTINCT FROM $2 AND name = $3
             RETURNING secret_id",
        )
        .bind(tenant_id.as_str())
        .bind(project_id)
        .bind(name)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("secret does not exist"))?;
        sqlx::query("UPDATE control_secrets SET deleted_at_ms = $2 WHERE secret_id = $1")
            .bind(&secret_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "secret.delete",
            "secret",
            name,
            "success",
            json!({ "secret_id": secret_id, "project_id": project_id }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn resolve_secret_for_node(
        &self,
        principal: &NodePrincipal,
        name: &str,
        now_ms: u64,
    ) -> Result<Zeroizing<Vec<u8>>, HarnessError> {
        validate_secret_name(name)?;
        let tenant_id = &principal.scope.tenant_id;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let row = sqlx::query(
            "SELECT secret.secret_id, secret.project_id, secret.version,
                    secret.nonce, secret.ciphertext
             FROM control_secret_heads AS head
             JOIN control_secrets AS secret ON secret.secret_id = head.secret_id
             WHERE head.tenant_id = $1 AND head.name = $2
               AND (head.project_id IS NULL OR head.project_id = $3)
               AND secret.deleted_at_ms IS NULL
             ORDER BY (head.project_id IS NOT NULL) DESC
             LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(name)
        .bind(principal.project_id.as_deref())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("secret does not exist"))?;
        let version = from_i64(
            row.try_get("version").map_err(database_error)?,
            "secret version",
        )?;
        let nonce: Vec<u8> = row.try_get("nonce").map_err(database_error)?;
        let nonce: [u8; 24] = nonce
            .try_into()
            .map_err(|_| HarnessError::execution("stored secret nonce has invalid length"))?;
        let encrypted = EncryptedSecret {
            nonce,
            ciphertext: row.try_get("ciphertext").map_err(database_error)?,
        };
        let project_id: Option<String> = row.try_get("project_id").map_err(database_error)?;
        let plaintext = self.cipher.decrypt(
            tenant_id.as_str(),
            project_id.as_deref(),
            name,
            version,
            &encrypted,
        )?;
        append_audit(
            &mut transaction,
            tenant_id,
            None,
            "node",
            "secret.resolve",
            "secret",
            name,
            "success",
            json!({
                "credential_id": principal.credential_id,
                "executor_id": principal.executor_id,
                "project_id": project_id,
                "version": version
            }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(plaintext)
    }

    pub async fn extension_inventory(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<ternilo_extension::ExtensionInventory, HarnessError> {
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::TenantRead,
        )
        .await?;
        let publisher_rows = sqlx::query(
            "SELECT trust, revoked, added_at_ms, updated_at_ms
             FROM control_extension_publishers WHERE tenant_id = $1 ORDER BY key_id",
        )
        .bind(tenant_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let plugin_rows = sqlx::query(
            "SELECT install_request, enabled, revoked, installed_at_ms, updated_at_ms
             FROM control_extension_packages
             WHERE tenant_id = $1 ORDER BY package_id, version",
        )
        .bind(tenant_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(ternilo_extension::ExtensionInventory {
            publishers: publisher_rows
                .iter()
                .map(trusted_extension_publisher_from_row)
                .collect::<Result<_, _>>()?,
            extensions: plugin_rows
                .iter()
                .map(installed_extension_from_row)
                .collect::<Result<_, _>>()?,
        })
    }

    pub async fn trust_extension_publisher(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        trust: ternilo_extension::PublisherTrust,
        now_ms: u64,
    ) -> Result<ternilo_extension::TrustedPublisher, HarnessError> {
        trust.validate()?;
        let now = to_i64(now_ms, "extension publisher timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::PluginManage,
        )
        .await?;
        if let Some(row) = sqlx::query(ternilo_storage::for_update(
            &transaction,
            "SELECT trust, revoked, added_at_ms, updated_at_ms
             FROM control_extension_publishers
             WHERE tenant_id = $1 AND key_id = $2",
            "SELECT trust, revoked, added_at_ms, updated_at_ms
             FROM control_extension_publishers
             WHERE tenant_id = $1 AND key_id = $2 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(&trust.key_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        {
            let existing = trusted_extension_publisher_from_row(&row)?;
            if existing.trust == trust && !existing.revoked {
                transaction.commit().await.map_err(database_error)?;
                return Ok(existing);
            }
            return Err(HarnessError::policy(
                "publisher key ids are immutable and revoked ids cannot be reused",
            ));
        }
        ternilo_storage::lock(
            &mut transaction,
            &(format!("{}:extension-publishers", tenant_id.as_str())),
        )
        .await?;
        let publisher_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM control_extension_publishers WHERE tenant_id = $1",
        )
        .bind(tenant_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if publisher_count >= MAX_EXTENSION_PUBLISHERS_PER_TENANT {
            return Err(HarnessError::policy(
                "tenant extension publisher inventory is full",
            ));
        }
        sqlx::query(
            "INSERT INTO control_extension_publishers
                (tenant_id, key_id, trust, revoked, added_at_ms, updated_at_ms, added_by)
             VALUES ($1, $2, $3, 0, $4, $4, $5)",
        )
        .bind(tenant_id.as_str())
        .bind(&trust.key_id)
        .bind(ternilo_storage::Json(&trust))
        .bind(now)
        .bind(actor.user_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "extension.publisher.trust",
            "extension_publisher",
            &trust.key_id,
            "success",
            json!({ "allowed_sources": trust.allowed_sources }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(ternilo_extension::TrustedPublisher {
            trust,
            revoked: false,
            added_at_ms: now_ms,
            updated_at_ms: now_ms,
        })
    }

    pub async fn revoke_extension_publisher(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        key_id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        require_bounded(key_id, "extension publisher key id", 160)?;
        let now = to_i64(now_ms, "extension publisher revocation timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::PluginManage,
        )
        .await?;
        let packages = sqlx::query(ternilo_storage::for_update(
            &transaction,
            "SELECT package_id, version FROM control_extension_packages
             WHERE tenant_id = $1 AND publisher_key_id = $2",
            "SELECT package_id, version FROM control_extension_packages
             WHERE tenant_id = $1 AND publisher_key_id = $2 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(key_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("package_id")
                    .map_err(database_error)?,
                row.try_get::<String, _>("version")
                    .map_err(database_error)?,
            ))
        })
        .collect::<Result<BTreeSet<_>, HarnessError>>()?;
        let changed = sqlx::query(
            "UPDATE control_extension_publishers
             SET revoked = 1, updated_at_ms = $3
             WHERE tenant_id = $1 AND key_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(key_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid("extension publisher does not exist"));
        }
        sqlx::query(
            "UPDATE control_extension_packages
             SET enabled = 0, revoked = 1, updated_at_ms = $3
             WHERE tenant_id = $1 AND publisher_key_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(key_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        remove_extension_mounts(&mut transaction, tenant_id, &packages, now).await?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "extension.publisher.revoke",
            "extension_publisher",
            key_id,
            "success",
            Value::Object(serde_json::Map::new()),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    #[allow(clippy::too_many_lines)]
    pub async fn install_extension(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        request: ternilo_extension::ExtensionInstallRequest,
        policy: &ternilo_extension::ExtensionHostPolicy,
        now_ms: u64,
    ) -> Result<ternilo_extension::InstalledExtension, HarnessError> {
        policy.validate_install(&request.bundle.manifest, &request.granted_capabilities)?;
        let publisher = {
            let mut transaction = self.database.begin().await?;
            set_tenant(&mut transaction, tenant_id).await?;
            require_action(
                &mut transaction,
                tenant_id,
                &actor.user_id,
                ControlAction::PluginManage,
            )
            .await?;
            let trust =
                sqlx::query_scalar::<_, ternilo_storage::Json<ternilo_extension::PublisherTrust>>(
                    "SELECT trust FROM control_extension_publishers
                 WHERE tenant_id = $1 AND key_id = $2 AND revoked = 0",
                )
                .bind(tenant_id.as_str())
                .bind(&request.bundle.manifest.publisher_key_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(database_error)?
                .ok_or_else(|| {
                    HarnessError::policy("extension publisher is not trusted or was revoked")
                })?
                .0;
            transaction.commit().await.map_err(database_error)?;
            trust
        };
        let verification_request = request.clone();
        let verification_publisher = publisher.clone();
        let verification_policy = policy.clone();
        tokio::task::spawn_blocking(move || {
            let directory = tempfile::tempdir().map_err(|error| {
                HarnessError::execution(format!(
                    "create temporary extension validation registry: {error}"
                ))
            })?;
            let registry = ternilo_extension::ExtensionRegistry::open(
                directory.path().join("registry"),
                verification_policy,
            )?;
            registry.trust_publisher(verification_publisher, 0)?;
            registry.install(verification_request, 0)?;
            Ok::<(), HarnessError>(())
        })
        .await
        .map_err(|error| {
            HarnessError::execution(format!("join extension verification: {error}"))
        })??;

        let manifest = &request.bundle.manifest;
        let now = to_i64(now_ms, "extension package installation timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::PluginManage,
        )
        .await?;
        let current_publisher =
            sqlx::query_scalar::<_, ternilo_storage::Json<ternilo_extension::PublisherTrust>>(
                ternilo_storage::for_update(&transaction,
                "SELECT trust FROM control_extension_publishers WHERE tenant_id = $1 AND key_id = $2 AND revoked = 0",
                "SELECT trust FROM control_extension_publishers WHERE tenant_id = $1 AND key_id = $2 AND revoked = 0 FOR SHARE",
            ),
            )
            .bind(tenant_id.as_str())
            .bind(&manifest.publisher_key_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            .ok_or_else(|| HarnessError::policy("extension publisher was revoked during install"))?
            .0;
        if current_publisher != publisher {
            return Err(HarnessError::policy(
                "extension publisher trust changed during install",
            ));
        }
        ternilo_storage::lock(
            &mut transaction,
            &(format!("{}:extension-packages", tenant_id.as_str())),
        )
        .await?;
        if let Some(row) = sqlx::query(ternilo_storage::for_update(
            &transaction,
            "SELECT install_request, enabled, revoked, installed_at_ms, updated_at_ms
             FROM control_extension_packages
             WHERE tenant_id = $1 AND package_id = $2 AND version = $3",
            "SELECT install_request, enabled, revoked, installed_at_ms, updated_at_ms
             FROM control_extension_packages
             WHERE tenant_id = $1 AND package_id = $2 AND version = $3 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(&manifest.package_id)
        .bind(&manifest.version)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        {
            let existing_request = row
                .try_get::<ternilo_storage::Json<ternilo_extension::ExtensionInstallRequest>, _>(
                    "install_request",
                )
                .map_err(database_error)?
                .0;
            let existing = installed_extension_from_row(&row)?;
            if existing_request == request && !existing.revoked {
                transaction.commit().await.map_err(database_error)?;
                return Ok(existing);
            }
            return Err(HarnessError::policy(
                "a different or revoked extension package already owns this id and version",
            ));
        }
        let install_identity = extension_install_identity(&request);
        let previous_identity = sqlx::query_scalar::<_, ternilo_storage::Json<Value>>(
            "SELECT metadata FROM control_audit_log
             WHERE tenant_id = $1 AND action = 'extension.package.install'
               AND resource_type = 'extension_package' AND resource_id = $2
               AND outcome = 'success'
             ORDER BY audit_sequence ASC LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(format!("{}@{}", manifest.package_id, manifest.version))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .map(|value| value.0);
        validate_extension_version_identity(previous_identity.as_ref(), &install_identity)?;
        let usage = sqlx::query(
            "SELECT COUNT(*) AS package_count,
                    COALESCE(SUM(octet_length(CAST(install_request AS TEXT))), 0) AS storage_bytes
             FROM control_extension_packages WHERE tenant_id = $1",
        )
        .bind(tenant_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let package_count: i64 = usage.try_get("package_count").map_err(database_error)?;
        let storage_bytes: i64 = usage.try_get("storage_bytes").map_err(database_error)?;
        let request_bytes = i64::try_from(serde_json::to_vec(&request).map_err(json_error)?.len())
            .map_err(|_| {
                HarnessError::policy("extension package storage size exceeds PostgreSQL")
            })?;
        if package_count >= MAX_EXTENSION_PACKAGES_PER_TENANT
            || storage_bytes.saturating_add(request_bytes) > MAX_EXTENSION_STORAGE_BYTES_PER_TENANT
        {
            return Err(HarnessError::policy(
                "tenant extension package count or storage quota is exhausted",
            ));
        }
        sqlx::query(
            "INSERT INTO control_extension_packages
                (tenant_id, package_id, version, publisher_key_id, install_request,
                 enabled, revoked, installed_at_ms, updated_at_ms, installed_by)
             VALUES ($1, $2, $3, $4, $5, 1, 0, $6, $6, $7)",
        )
        .bind(tenant_id.as_str())
        .bind(&manifest.package_id)
        .bind(&manifest.version)
        .bind(&manifest.publisher_key_id)
        .bind(ternilo_storage::Json(&request))
        .bind(now)
        .bind(actor.user_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "extension.package.install",
            "extension_package",
            &format!("{}@{}", manifest.package_id, manifest.version),
            "success",
            install_identity,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(ternilo_extension::InstalledExtension {
            manifest: request.bundle.manifest,
            granted_capabilities: request.granted_capabilities,
            enabled: true,
            revoked: false,
            installed_at_ms: now_ms,
            updated_at_ms: now_ms,
        })
    }

    pub async fn set_extension_enabled(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        package_id: &str,
        version: &str,
        enabled: bool,
        now_ms: u64,
    ) -> Result<ternilo_extension::InstalledExtension, HarnessError> {
        let now = to_i64(now_ms, "extension package update timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::PluginManage,
        )
        .await?;
        let row = sqlx::query(ternilo_storage::for_update(
            &transaction,
            "SELECT plugin.install_request, plugin.enabled, plugin.revoked,
                    plugin.installed_at_ms, plugin.updated_at_ms,
                    publisher.revoked AS publisher_revoked
             FROM control_extension_packages AS plugin
             JOIN control_extension_publishers AS publisher
               ON publisher.tenant_id = plugin.tenant_id
              AND publisher.key_id = plugin.publisher_key_id
             WHERE plugin.tenant_id = $1 AND plugin.package_id = $2
               AND plugin.version = $3",
            "SELECT plugin.install_request, plugin.enabled, plugin.revoked,
                    plugin.installed_at_ms, plugin.updated_at_ms,
                    publisher.revoked AS publisher_revoked
             FROM control_extension_packages AS plugin
             JOIN control_extension_publishers AS publisher
               ON publisher.tenant_id = plugin.tenant_id
              AND publisher.key_id = plugin.publisher_key_id
             WHERE plugin.tenant_id = $1 AND plugin.package_id = $2
               AND plugin.version = $3 FOR UPDATE OF plugin",
        ))
        .bind(tenant_id.as_str())
        .bind(package_id)
        .bind(version)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("extension package does not exist"))?;
        if enabled
            && ((row.try_get::<i64, _>("revoked").map_err(database_error)? != 0)
                || (row
                    .try_get::<i64, _>("publisher_revoked")
                    .map_err(database_error)?
                    != 0))
        {
            return Err(HarnessError::policy(
                "revoked extension packages or publishers cannot be re-enabled",
            ));
        }
        sqlx::query(
            "UPDATE control_extension_packages SET enabled = $4, updated_at_ms = $5
             WHERE tenant_id = $1 AND package_id = $2 AND version = $3",
        )
        .bind(tenant_id.as_str())
        .bind(package_id)
        .bind(version)
        .bind(i64::from(enabled))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if !enabled {
            remove_extension_mounts(
                &mut transaction,
                tenant_id,
                &[(package_id.to_owned(), version.to_owned())]
                    .into_iter()
                    .collect(),
                now,
            )
            .await?;
        }
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            if enabled {
                "extension.package.enable"
            } else {
                "extension.package.disable"
            },
            "extension_package",
            &format!("{package_id}@{version}"),
            "success",
            Value::Object(serde_json::Map::new()),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        let mut installed = installed_extension_from_row(&row)?;
        installed.enabled = enabled;
        installed.updated_at_ms = now_ms;
        Ok(installed)
    }

    pub async fn revoke_extension(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        package_id: &str,
        version: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let now = to_i64(now_ms, "extension package revocation timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::PluginManage,
        )
        .await?;
        let changed = sqlx::query(
            "UPDATE control_extension_packages
             SET enabled = 0, revoked = 1, updated_at_ms = $4
             WHERE tenant_id = $1 AND package_id = $2 AND version = $3",
        )
        .bind(tenant_id.as_str())
        .bind(package_id)
        .bind(version)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid("extension package does not exist"));
        }
        remove_extension_mounts(
            &mut transaction,
            tenant_id,
            &[(package_id.to_owned(), version.to_owned())]
                .into_iter()
                .collect(),
            now,
        )
        .await?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "extension.package.revoke",
            "extension_package",
            &format!("{package_id}@{version}"),
            "success",
            Value::Object(serde_json::Map::new()),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn uninstall_extension(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        package_id: &str,
        version: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let now = to_i64(now_ms, "extension package uninstall timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::PluginManage,
        )
        .await?;
        let revoked = sqlx::query_scalar::<_, i64>(ternilo_storage::for_update(
            &transaction,
            "SELECT revoked FROM control_extension_packages
             WHERE tenant_id = $1 AND package_id = $2 AND version = $3",
            "SELECT revoked FROM control_extension_packages
             WHERE tenant_id = $1 AND package_id = $2 AND version = $3 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(package_id)
        .bind(version)
        .fetch_optional(&mut *transaction)
        .await
        .map(|value| value.map(|value| value != 0))
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("extension package does not exist"))?;
        if revoked {
            return Err(HarnessError::policy(
                "revoked extension package versions cannot be uninstalled",
            ));
        }
        sqlx::query(
            "DELETE FROM control_extension_packages
             WHERE tenant_id = $1 AND package_id = $2 AND version = $3",
        )
        .bind(tenant_id.as_str())
        .bind(package_id)
        .bind(version)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        remove_extension_mounts(
            &mut transaction,
            tenant_id,
            &[(package_id.to_owned(), version.to_owned())]
                .into_iter()
                .collect(),
            now,
        )
        .await?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "extension.package.uninstall",
            "extension_package",
            &format!("{package_id}@{version}"),
            "success",
            Value::Object(serde_json::Map::new()),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn resolve_extensions(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        profile: &ternilo_protocol::Profile,
        policy: &ternilo_extension::ExtensionHostPolicy,
    ) -> Result<Vec<ternilo_extension::ExtensionDistribution>, HarnessError> {
        let mounts_by_reference = ternilo_extension::unique_extension_mounts(profile)?
            .into_iter()
            .map(|mount| ((mount.package_id, mount.version), mount.settings))
            .collect::<BTreeMap<_, _>>();
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::RunReserve,
        )
        .await?;
        let mut distributions = Vec::with_capacity(mounts_by_reference.len());
        for (reference, settings) in mounts_by_reference {
            let row = sqlx::query(
                "SELECT publisher.trust, plugin.install_request
                 FROM control_extension_packages AS plugin
                 JOIN control_extension_publishers AS publisher
                   ON publisher.tenant_id = plugin.tenant_id
                  AND publisher.key_id = plugin.publisher_key_id
                 WHERE plugin.tenant_id = $1 AND plugin.package_id = $2
                   AND plugin.version = $3 AND plugin.enabled = 1 AND plugin.revoked = 0
                   AND publisher.revoked = 0",
            )
            .bind(tenant_id.as_str())
            .bind(&reference.0)
            .bind(&reference.1)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            .ok_or_else(|| {
                HarnessError::policy(format!(
                    "cloud extension package {}@{} is unavailable or revoked",
                    reference.0, reference.1
                ))
            })?;
            let distribution = extension_distribution_from_row(&row)?;
            policy.validate_install(
                &distribution.install.bundle.manifest,
                &distribution.install.granted_capabilities,
            )?;
            ternilo_extension::verify_bundle(
                &distribution.install.bundle,
                &distribution.publisher,
                policy.max_payload_bytes,
            )?;
            ternilo_extension::validate_extension_settings(
                &distribution.install.bundle.manifest,
                &settings,
            )?;
            distributions.push(distribution);
        }
        ternilo_extension::validate_extension_tool_name_uniqueness(
            distributions
                .iter()
                .map(|distribution| &distribution.install.bundle.manifest),
        )?;
        ternilo_extension::validate_extension_command_name_uniqueness(
            distributions
                .iter()
                .map(|distribution| &distribution.install.bundle.manifest),
        )?;
        transaction.commit().await.map_err(database_error)?;
        Ok(distributions)
    }

    pub async fn list_audit(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, HarnessError> {
        if limit == 0 || limit > 1_000 {
            return Err(HarnessError::invalid(
                "audit limit must be between 1 and 1000",
            ));
        }
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::AuditRead,
        )
        .await?;
        let rows = sqlx::query(
            "SELECT audit_id, actor_user_id, actor_kind, action, resource_type,
                    resource_id, outcome, metadata, previous_hash, entry_hash, occurred_at_ms
             FROM control_audit_log WHERE tenant_id = $1
             ORDER BY audit_sequence ASC LIMIT $2",
        )
        .bind(tenant_id.as_str())
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        decode_and_verify_audit(tenant_id, rows)
    }
}

fn extension_install_identity(request: &ternilo_extension::ExtensionInstallRequest) -> Value {
    json!({
        "publisher_key_id": request.bundle.manifest.publisher_key_id,
        "payload_sha256": request.bundle.manifest.payload_sha256,
        "signature_base64": request.bundle.signature_base64,
        "granted_capabilities": request.granted_capabilities,
    })
}

fn validate_extension_version_identity(
    previous: Option<&Value>,
    current: &Value,
) -> Result<(), HarnessError> {
    if previous.is_some_and(|identity| identity != current) {
        return Err(HarnessError::policy(
            "extension package id and version are permanently bound to their first signed bundle and capability grants",
        ));
    }
    Ok(())
}

async fn rotate_secret_master_key_in_database(
    database: &Database,
    current_cipher: &SecretCipher,
    next_cipher: &SecretCipher,
) -> Result<u64, HarnessError> {
    let mut transaction = database.begin().await?;
    if database.backend() == Backend::Postgres {
        sqlx::query(
            "LOCK TABLE control_secrets, control_user_credentials,
                    control_user_credential_records, control_model_providers,
                    control_authentication_settings, control_oidc_sessions,
                    control_mfa_factors, control_mfa_oidc_challenges IN ACCESS EXCLUSIVE MODE",
        )
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    }

    let secrets = rotate_project_secrets(&mut transaction, current_cipher, next_cipher).await?;
    let credentials =
        rotate_user_credentials(&mut transaction, current_cipher, next_cipher).await?;
    let records =
        rotate_user_credential_records(&mut transaction, current_cipher, next_cipher).await?;
    let model_credentials =
        crate::model_store::rotate_model_credentials(&mut transaction, current_cipher, next_cipher)
            .await?;
    let rotated = secrets
        .checked_add(credentials)
        .and_then(|count| count.checked_add(records))
        .and_then(|count| count.checked_add(model_credentials))
        .ok_or_else(|| HarnessError::execution("rotated secret count overflow"))?;
    let rotated = rotated
        .checked_add(
            crate::authentication_settings::rotate(&mut transaction, current_cipher, next_cipher)
                .await?,
        )
        .ok_or_else(|| HarnessError::execution("rotated secret count overflow"))?;
    let rotated = rotated
        .checked_add(
            crate::oidc_sessions::rotate(&mut transaction, current_cipher, next_cipher).await?,
        )
        .ok_or_else(|| HarnessError::execution("rotated secret count overflow"))?;
    let rotated = rotated
        .checked_add(
            crate::mfa::rotation::rotate(&mut transaction, current_cipher, next_cipher).await?,
        )
        .ok_or_else(|| HarnessError::execution("rotated secret count overflow"))?;
    transaction.commit().await.map_err(database_error)?;
    Ok(rotated)
}

const SECRET_ROTATION_BATCH_SIZE: i64 = 128;

async fn rotate_project_secrets(
    transaction: &mut ternilo_storage::Transaction,
    current_cipher: &SecretCipher,
    next_cipher: &SecretCipher,
) -> Result<u64, HarnessError> {
    let mut cursor: Option<String> = None;
    let mut rotated = 0_u64;
    loop {
        let rows = sqlx::query(
            "SELECT secret_id, tenant_id, project_id, name, version, nonce, ciphertext
             FROM control_secrets
             WHERE CAST($1 AS TEXT) IS NULL OR secret_id > $1
             ORDER BY secret_id
             LIMIT $2",
        )
        .bind(cursor.as_deref())
        .bind(SECRET_ROTATION_BATCH_SIZE)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
        if rows.is_empty() {
            break;
        }

        for row in &rows {
            let secret_id: String = row.try_get("secret_id").map_err(database_error)?;
            let tenant_id: String = row.try_get("tenant_id").map_err(database_error)?;
            let project_id: Option<String> = row.try_get("project_id").map_err(database_error)?;
            let name: String = row.try_get("name").map_err(database_error)?;
            let version = from_i64(
                row.try_get("version").map_err(database_error)?,
                "secret version",
            )?;
            let nonce: Vec<u8> = row.try_get("nonce").map_err(database_error)?;
            let nonce: [u8; 24] = nonce
                .try_into()
                .map_err(|_| HarnessError::execution("stored secret nonce has invalid length"))?;
            let encrypted = EncryptedSecret {
                nonce,
                ciphertext: row.try_get("ciphertext").map_err(database_error)?,
            };
            let plaintext = current_cipher.decrypt(
                &tenant_id,
                project_id.as_deref(),
                &name,
                version,
                &encrypted,
            )?;
            let replacement = next_cipher.encrypt(
                &tenant_id,
                project_id.as_deref(),
                &name,
                version,
                &plaintext,
            )?;
            let updated = sqlx::query(
                "UPDATE control_secrets SET nonce = $2, ciphertext = $3 WHERE secret_id = $1",
            )
            .bind(&secret_id)
            .bind(replacement.nonce.to_vec())
            .bind(replacement.ciphertext)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
            if updated.rows_affected() != 1 {
                return Err(HarnessError::execution(
                    "secret disappeared during master-key rotation",
                ));
            }
            rotated = rotated
                .checked_add(1)
                .ok_or_else(|| HarnessError::execution("rotated secret count overflow"))?;
            cursor = Some(secret_id);
        }
    }
    Ok(rotated)
}

async fn rotate_user_credentials(
    transaction: &mut ternilo_storage::Transaction,
    current_cipher: &SecretCipher,
    next_cipher: &SecretCipher,
) -> Result<u64, HarnessError> {
    let mut credential_cursor: Option<(String, String, String)> = None;
    let mut rotated = 0_u64;
    loop {
        let rows = sqlx::query(
            "SELECT tenant_id, user_id, name, version, nonce, ciphertext
             FROM control_user_credentials
             WHERE CAST($1 AS TEXT) IS NULL OR (tenant_id, user_id, name) > ($1, $2, $3)
             ORDER BY tenant_id, user_id, name
             LIMIT $4",
        )
        .bind(credential_cursor.as_ref().map(|cursor| cursor.0.as_str()))
        .bind(credential_cursor.as_ref().map(|cursor| cursor.1.as_str()))
        .bind(credential_cursor.as_ref().map(|cursor| cursor.2.as_str()))
        .bind(SECRET_ROTATION_BATCH_SIZE)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let tenant_id: String = row.try_get("tenant_id").map_err(database_error)?;
            let user_id: String = row.try_get("user_id").map_err(database_error)?;
            let name: String = row.try_get("name").map_err(database_error)?;
            let version = from_i64(
                row.try_get("version").map_err(database_error)?,
                "user credential version",
            )?;
            let encrypted = encrypted_secret_from_row(row, "user credential")?;
            let plaintext =
                current_cipher.decrypt(&tenant_id, Some(&user_id), &name, version, &encrypted)?;
            let replacement =
                next_cipher.encrypt(&tenant_id, Some(&user_id), &name, version, &plaintext)?;
            let updated = sqlx::query(
                "UPDATE control_user_credentials SET nonce = $4, ciphertext = $5
                 WHERE tenant_id = $1 AND user_id = $2 AND name = $3",
            )
            .bind(&tenant_id)
            .bind(&user_id)
            .bind(&name)
            .bind(replacement.nonce.to_vec())
            .bind(replacement.ciphertext)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
            if updated.rows_affected() != 1 {
                return Err(HarnessError::execution(
                    "user credential disappeared during master-key rotation",
                ));
            }
            rotated = rotated
                .checked_add(1)
                .ok_or_else(|| HarnessError::execution("rotated secret count overflow"))?;
            credential_cursor = Some((tenant_id, user_id, name));
        }
    }
    Ok(rotated)
}

async fn rotate_user_credential_records(
    transaction: &mut ternilo_storage::Transaction,
    current_cipher: &SecretCipher,
    next_cipher: &SecretCipher,
) -> Result<u64, HarnessError> {
    let mut record_cursor: Option<(String, String, String)> = None;
    let mut rotated = 0_u64;
    loop {
        let rows = sqlx::query(
            "SELECT tenant_id, user_id, record_key, version, nonce, ciphertext
             FROM control_user_credential_records
             WHERE CAST($1 AS TEXT) IS NULL OR (tenant_id, user_id, record_key) > ($1, $2, $3)
             ORDER BY tenant_id, user_id, record_key
             LIMIT $4",
        )
        .bind(record_cursor.as_ref().map(|cursor| cursor.0.as_str()))
        .bind(record_cursor.as_ref().map(|cursor| cursor.1.as_str()))
        .bind(record_cursor.as_ref().map(|cursor| cursor.2.as_str()))
        .bind(SECRET_ROTATION_BATCH_SIZE)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let tenant_id: String = row.try_get("tenant_id").map_err(database_error)?;
            let user_id: String = row.try_get("user_id").map_err(database_error)?;
            let key: String = row.try_get("record_key").map_err(database_error)?;
            let version = from_i64(
                row.try_get("version").map_err(database_error)?,
                "user credential record version",
            )?;
            let encrypted = encrypted_secret_from_row(row, "user credential record")?;
            let plaintext =
                current_cipher.decrypt(&tenant_id, Some(&user_id), &key, version, &encrypted)?;
            let replacement =
                next_cipher.encrypt(&tenant_id, Some(&user_id), &key, version, &plaintext)?;
            let updated = sqlx::query(
                "UPDATE control_user_credential_records SET nonce = $4, ciphertext = $5
                 WHERE tenant_id = $1 AND user_id = $2 AND record_key = $3",
            )
            .bind(&tenant_id)
            .bind(&user_id)
            .bind(&key)
            .bind(replacement.nonce.to_vec())
            .bind(replacement.ciphertext)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
            if updated.rows_affected() != 1 {
                return Err(HarnessError::execution(
                    "user credential record disappeared during master-key rotation",
                ));
            }
            rotated = rotated
                .checked_add(1)
                .ok_or_else(|| HarnessError::execution("rotated secret count overflow"))?;
            record_cursor = Some((tenant_id, user_id, key));
        }
    }
    Ok(rotated)
}

async fn remove_extension_mounts(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    packages: &BTreeSet<(String, String)>,
    now: i64,
) -> Result<(), HarnessError> {
    if packages.is_empty() {
        return Ok(());
    }

    let preset_rows = sqlx::query(ternilo_storage::for_update(
        transaction,
        "SELECT user_id, preset_id, document_json
         FROM control_user_agent_presets WHERE tenant_id = $1",
        "SELECT user_id, preset_id, document_json
         FROM control_user_agent_presets WHERE tenant_id = $1 FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    for row in preset_rows {
        let mut document = row
            .try_get::<ternilo_storage::Json<AgentPresetDocument>, _>("document_json")
            .map_err(database_error)?
            .0;
        if remove_extension_entries(&mut document.profile.plugins, packages) {
            sqlx::query(
                "UPDATE control_user_agent_presets
                 SET document_json = $4, updated_at_ms = CASE WHEN updated_at_ms > $5 THEN updated_at_ms ELSE $5 END
                 WHERE tenant_id = $1 AND user_id = $2 AND preset_id = $3",
            )
            .bind(tenant_id.as_str())
            .bind(
                row.try_get::<String, _>("user_id")
                    .map_err(database_error)?,
            )
            .bind(
                row.try_get::<String, _>("preset_id")
                    .map_err(database_error)?,
            )
            .bind(ternilo_storage::Json(document))
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
        }
    }

    let session_rows = sqlx::query(ternilo_storage::for_update(
        transaction,
        "SELECT session_id, profile_plugins
         FROM cloud_sessions WHERE tenant_id = $1",
        "SELECT session_id, profile_plugins
         FROM cloud_sessions WHERE tenant_id = $1 FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    for row in session_rows {
        let mut plugins = row
            .try_get::<ternilo_storage::Json<Vec<PluginEntry>>, _>("profile_plugins")
            .map_err(database_error)?
            .0;
        if remove_extension_entries(&mut plugins, packages) {
            sqlx::query(
                "UPDATE cloud_sessions
                 SET profile_plugins = $3, updated_at_ms = CASE WHEN updated_at_ms > $4 THEN updated_at_ms ELSE $4 END
                 WHERE tenant_id = $1 AND session_id = $2",
            )
            .bind(tenant_id.as_str())
            .bind(
                row.try_get::<String, _>("session_id")
                    .map_err(database_error)?,
            )
            .bind(ternilo_storage::Json(plugins))
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
        }
    }
    Ok(())
}

fn remove_extension_entries(
    entries: &mut Vec<PluginEntry>,
    packages: &BTreeSet<(String, String)>,
) -> bool {
    let before = entries.len();
    entries.retain(|entry| {
        entry.kind != ternilo_extension::EXTENSION_PACKAGE_KIND
            || !packages.iter().any(|(package_id, version)| {
                entry.config.get("package_id").and_then(Value::as_str) == Some(package_id.as_str())
                    && entry.config.get("version").and_then(Value::as_str) == Some(version.as_str())
            })
    });
    entries.len() != before
}

fn encrypted_secret_from_row(row: &AnyRow, label: &str) -> Result<EncryptedSecret, HarnessError> {
    let nonce: Vec<u8> = row.try_get("nonce").map_err(database_error)?;
    Ok(EncryptedSecret {
        nonce: nonce.try_into().map_err(|_| {
            HarnessError::execution(format!("stored {label} nonce has invalid length"))
        })?,
        ciphertext: row.try_get("ciphertext").map_err(database_error)?,
    })
}

fn trusted_extension_publisher_from_row(
    row: &sqlx::any::AnyRow,
) -> Result<ternilo_extension::TrustedPublisher, HarnessError> {
    Ok(ternilo_extension::TrustedPublisher {
        trust: row
            .try_get::<ternilo_storage::Json<ternilo_extension::PublisherTrust>, _>("trust")
            .map_err(database_error)?
            .0,
        revoked: row.try_get::<i64, _>("revoked").map_err(database_error)? != 0,
        added_at_ms: from_i64(
            row.try_get("added_at_ms").map_err(database_error)?,
            "extension publisher creation timestamp",
        )?,
        updated_at_ms: from_i64(
            row.try_get("updated_at_ms").map_err(database_error)?,
            "extension publisher update timestamp",
        )?,
    })
}

fn installed_extension_from_row(
    row: &sqlx::any::AnyRow,
) -> Result<ternilo_extension::InstalledExtension, HarnessError> {
    let request = row
        .try_get::<ternilo_storage::Json<ternilo_extension::ExtensionInstallRequest>, _>(
            "install_request",
        )
        .map_err(database_error)?
        .0;
    Ok(ternilo_extension::InstalledExtension {
        manifest: request.bundle.manifest,
        granted_capabilities: request.granted_capabilities,
        enabled: row.try_get::<i64, _>("enabled").map_err(database_error)? != 0,
        revoked: row.try_get::<i64, _>("revoked").map_err(database_error)? != 0,
        installed_at_ms: from_i64(
            row.try_get("installed_at_ms").map_err(database_error)?,
            "extension package installation timestamp",
        )?,
        updated_at_ms: from_i64(
            row.try_get("updated_at_ms").map_err(database_error)?,
            "extension package update timestamp",
        )?,
    })
}

fn extension_distribution_from_row(
    row: &sqlx::any::AnyRow,
) -> Result<ternilo_extension::ExtensionDistribution, HarnessError> {
    Ok(ternilo_extension::ExtensionDistribution {
        publisher: row
            .try_get::<ternilo_storage::Json<ternilo_extension::PublisherTrust>, _>("trust")
            .map_err(database_error)?
            .0,
        install: row
            .try_get::<ternilo_storage::Json<ternilo_extension::ExtensionInstallRequest>, _>(
                "install_request",
            )
            .map_err(database_error)?
            .0,
    })
}

pub(crate) async fn token_tenant(
    transaction: &mut ternilo_storage::Transaction,
    hash: &[u8],
    enrollment: bool,
) -> Result<TenantId, HarnessError> {
    let sql = match (ternilo_storage::backend(transaction), enrollment) {
        (Backend::Sqlite, true) => {
            "SELECT tenant_id FROM control_executor_enrollments WHERE token_hash = $1"
        }
        (Backend::Sqlite, false) => {
            "SELECT tenant_id FROM control_node_credentials WHERE token_hash = $1"
        }
        (Backend::Postgres, true) => "SELECT ternilo_enrollment_tenant($1)",
        (Backend::Postgres, false) => "SELECT ternilo_node_credential_tenant($1)",
    };
    sqlx::query_scalar::<_, Option<String>>(sql)
        .bind(hash)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .flatten()
        .map(TenantId::new)
        .ok_or_else(|| HarnessError::policy("node token is invalid"))
}

pub(crate) async fn set_tenant(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
) -> Result<(), HarnessError> {
    tenant_id.validate()?;
    ternilo_storage::set_tenant_scope(transaction, tenant_id).await
}

pub(crate) async fn require_action(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    action: ControlAction,
) -> Result<TenantRole, HarnessError> {
    let row = sqlx::query(
        "SELECT membership.role, account.status FROM control_memberships membership
         JOIN control_users account ON account.user_id = membership.user_id
         WHERE membership.tenant_id = $1 AND membership.user_id = $2",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::policy("user is not a member of this tenant"))?;
    crate::AccountStatus::parse(&row.try_get::<String, _>("status").map_err(database_error)?)?
        .require_active()?;
    let role = TenantRole::parse(&row.try_get::<String, _>("role").map_err(database_error)?)?;
    if !role.allows(action) {
        return Err(HarnessError::policy(format!(
            "tenant role {} does not allow {action:?}",
            role.as_str()
        )));
    }
    Ok(role)
}

async fn insert_quota(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    quota: &TenantQuota,
    now_ms: i64,
) -> Result<(), HarnessError> {
    sqlx::query(
        "INSERT INTO control_quotas
            (tenant_id, max_nodes, max_concurrent_runs, monthly_model_tokens,
             max_secrets, updated_at_ms, updated_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (tenant_id) DO UPDATE SET
            max_nodes = EXCLUDED.max_nodes,
            max_concurrent_runs = EXCLUDED.max_concurrent_runs,
            monthly_model_tokens = EXCLUDED.monthly_model_tokens,
            max_secrets = EXCLUDED.max_secrets,
            updated_at_ms = EXCLUDED.updated_at_ms,
            updated_by = EXCLUDED.updated_by",
    )
    .bind(tenant_id.as_str())
    .bind(to_i32(quota.max_nodes, "node quota")?)
    .bind(to_i32(quota.max_concurrent_runs, "concurrent-run quota")?)
    .bind(to_i64(quota.monthly_model_tokens, "monthly token quota")?)
    .bind(to_i32(quota.max_secrets, "secret quota")?)
    .bind(now_ms)
    .bind(user_id.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_audit(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    actor_user_id: Option<&UserId>,
    actor_kind: &str,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    outcome: &str,
    metadata: Value,
    occurred_at_ms: u64,
) -> Result<String, HarnessError> {
    require_bounded(actor_kind, "audit actor kind", 32)?;
    require_bounded(action, "audit action", 128)?;
    require_bounded(resource_type, "audit resource type", 128)?;
    require_bounded(resource_id, "audit resource id", 512)?;
    require_bounded(outcome, "audit outcome", 32)?;
    ternilo_storage::lock(transaction, tenant_id.as_str()).await?;
    let previous_hash = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT entry_hash FROM control_audit_log
         WHERE tenant_id = $1 ORDER BY audit_sequence DESC LIMIT 1",
    )
    .bind(tenant_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    let audit_id = random_identifier("aud");
    let payload = audit_payload(
        &audit_id,
        tenant_id.as_str(),
        actor_user_id.map(UserId::as_str),
        actor_kind,
        action,
        resource_type,
        resource_id,
        outcome,
        &metadata,
        occurred_at_ms,
    )?;
    let entry_hash = chained_hash(previous_hash.as_deref(), &payload);
    sqlx::query(
        "INSERT INTO control_audit_log
            (audit_id, tenant_id, actor_user_id, actor_kind, action, resource_type,
             resource_id, outcome, metadata, previous_hash, entry_hash, occurred_at_ms)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(&audit_id)
    .bind(tenant_id.as_str())
    .bind(actor_user_id.map(UserId::as_str))
    .bind(actor_kind)
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(outcome)
    .bind(ternilo_storage::Json(metadata))
    .bind(previous_hash)
    .bind(entry_hash.to_vec())
    .bind(to_i64(occurred_at_ms, "audit timestamp")?)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(audit_id)
}

#[allow(clippy::too_many_arguments)]
fn audit_payload(
    audit_id: &str,
    tenant_id: &str,
    actor_user_id: Option<&str>,
    actor_kind: &str,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    outcome: &str,
    metadata: &Value,
    occurred_at_ms: u64,
) -> Result<Vec<u8>, HarnessError> {
    let mut payload = json!({
        "audit_id": audit_id,
        "tenant_id": tenant_id,
        "actor_user_id": actor_user_id,
        "actor_kind": actor_kind,
        "action": action,
        "resource_type": resource_type,
        "resource_id": resource_id,
        "outcome": outcome,
        "metadata": metadata,
        "occurred_at_ms": occurred_at_ms,
    });
    sort_json_keys(&mut payload);
    serde_json::to_vec(&payload).map_err(json_error)
}

fn sort_json_keys(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(sort_json_keys),
        Value::Object(values) => {
            let mut entries = std::mem::take(values).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for (_, value) in &mut entries {
                sort_json_keys(value);
            }
            values.extend(entries);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn decode_and_verify_audit(
    tenant_id: &TenantId,
    rows: Vec<sqlx::any::AnyRow>,
) -> Result<Vec<AuditEntry>, HarnessError> {
    let mut previous: Option<Vec<u8>> = None;
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let audit_id: String = row.try_get("audit_id").map_err(database_error)?;
        let actor_user: Option<String> = row.try_get("actor_user_id").map_err(database_error)?;
        let actor_kind: String = row.try_get("actor_kind").map_err(database_error)?;
        let action: String = row.try_get("action").map_err(database_error)?;
        let resource_type: String = row.try_get("resource_type").map_err(database_error)?;
        let resource_id: String = row.try_get("resource_id").map_err(database_error)?;
        let outcome: String = row.try_get("outcome").map_err(database_error)?;
        let metadata: ternilo_storage::Json<Value> =
            row.try_get("metadata").map_err(database_error)?;
        let stored_previous: Option<Vec<u8>> =
            row.try_get("previous_hash").map_err(database_error)?;
        let entry_hash: Vec<u8> = row.try_get("entry_hash").map_err(database_error)?;
        let occurred_at_ms = from_i64(
            row.try_get("occurred_at_ms").map_err(database_error)?,
            "audit timestamp",
        )?;
        if stored_previous.as_deref() != previous.as_deref() {
            return Err(HarnessError::policy("audit hash chain is discontinuous"));
        }
        let payload = audit_payload(
            &audit_id,
            tenant_id.as_str(),
            actor_user.as_deref(),
            &actor_kind,
            &action,
            &resource_type,
            &resource_id,
            &outcome,
            &metadata.0,
            occurred_at_ms,
        )?;
        let expected = chained_hash(previous.as_deref(), &payload);
        if entry_hash.as_slice() != expected {
            return Err(HarnessError::policy("audit entry hash verification failed"));
        }
        previous = Some(entry_hash.clone());
        entries.push(AuditEntry {
            audit_id,
            tenant_id: tenant_id.clone(),
            actor_user_id: actor_user.map(UserId::new),
            actor_kind,
            action,
            resource_type,
            resource_id,
            outcome,
            metadata: metadata.0,
            occurred_at_ms,
            entry_hash_hex: hex(&entry_hash),
        });
    }
    Ok(entries)
}

fn quota_from_row(row: &sqlx::any::AnyRow) -> Result<TenantQuota, HarnessError> {
    Ok(TenantQuota {
        max_nodes: from_i32(
            row.try_get("max_nodes").map_err(database_error)?,
            "node quota",
        )?,
        max_concurrent_runs: from_i32(
            row.try_get("max_concurrent_runs").map_err(database_error)?,
            "concurrent-run quota",
        )?,
        monthly_model_tokens: from_i64(
            row.try_get("monthly_model_tokens")
                .map_err(database_error)?,
            "monthly token quota",
        )?,
        max_secrets: from_i32(
            row.try_get("max_secrets").map_err(database_error)?,
            "secret quota",
        )?,
    })
}

fn secret_metadata_from_row(row: &sqlx::any::AnyRow) -> Result<SecretMetadata, HarnessError> {
    Ok(SecretMetadata {
        secret_id: row.try_get("secret_id").map_err(database_error)?,
        project_id: row.try_get("project_id").map_err(database_error)?,
        name: row.try_get("name").map_err(database_error)?,
        version: from_i64(
            row.try_get("version").map_err(database_error)?,
            "secret version",
        )?,
        created_at_ms: from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "secret creation timestamp",
        )?,
    })
}

pub(crate) fn workspace_from_row(row: &sqlx::any::AnyRow) -> Result<WorkspaceRecord, HarnessError> {
    Ok(WorkspaceRecord {
        tenant_id: TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        ),
        workspace_id: WorkspaceId::new(
            row.try_get::<String, _>("workspace_id")
                .map_err(database_error)?,
        ),
        project_id: row.try_get("project_id").map_err(database_error)?,
        owner_user_id: UserId::new(
            row.try_get::<String, _>("owner_user_id")
                .map_err(database_error)?,
        ),
        name: row.try_get("name").map_err(database_error)?,
        placement: WorkspacePlacement::parse(
            &row.try_get::<String, _>("placement")
                .map_err(database_error)?,
        )?,
        storage: WorkspaceStorage::parse(
            &row.try_get::<String, _>("storage")
                .map_err(database_error)?,
        )?,
        executor_id: row
            .try_get::<Option<String>, _>("executor_id")
            .map_err(database_error)?
            .map(ExecutorId::new),
        executor_workspace_id: row
            .try_get::<Option<String>, _>("executor_workspace_id")
            .map_err(database_error)?
            .map(WorkspaceId::new),
        created_at_ms: from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "workspace creation timestamp",
        )?,
        updated_at_ms: from_i64(
            row.try_get("updated_at_ms").map_err(database_error)?,
            "workspace update timestamp",
        )?,
    })
}

fn validate_slug(slug: &str) -> Result<(), HarnessError> {
    if slug.is_empty()
        || slug.len() > 63
        || slug.starts_with('-')
        || slug.ends_with('-')
        || slug
            .bytes()
            .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && byte != b'-')
    {
        Err(HarnessError::invalid(
            "tenant slug must use 1 to 63 lowercase ASCII letters, digits, or interior hyphens",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn executor_record(row: &AnyRow) -> Result<ExecutorRecord, HarnessError> {
    Ok(ExecutorRecord {
        management: crate::computer_management::management_record(row)?,
        executor_id: ExecutorId::new(
            row.try_get::<String, _>("executor_id")
                .map_err(database_error)?,
        ),
        project_id: row.try_get("project_id").map_err(database_error)?,
        state: row.try_get("state").map_err(database_error)?,
        enrolled_at_ms: from_i64(
            row.try_get("enrolled_at_ms").map_err(database_error)?,
            "executor enrollment timestamp",
        )?,
        last_seen_at_ms: row
            .try_get::<Option<i64>, _>("last_seen_at_ms")
            .map_err(database_error)?
            .map(|value| from_i64(value, "executor last-seen timestamp"))
            .transpose()?,
    })
}

fn validate_secret_name(name: &str) -> Result<(), HarnessError> {
    if name.is_empty()
        || name.len() > 128
        || name
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-' | b'.'))
    {
        Err(HarnessError::invalid(
            "secret name must use 1 to 128 ASCII letters, digits, dots, underscores, or hyphens",
        ))
    } else {
        Ok(())
    }
}

fn duration_ms(duration: Duration) -> Result<u64, HarnessError> {
    duration
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::invalid("duration exceeds u64 milliseconds"))
}

pub(crate) fn to_i64(value: u64, label: &str) -> Result<i64, HarnessError> {
    i64::try_from(value)
        .map_err(|_| HarnessError::execution(format!("{label} exceeds PostgreSQL bigint")))
}

pub(crate) fn from_i64(value: i64, label: &str) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution(format!("{label} is negative")))
}

fn to_i32(value: u32, label: &str) -> Result<i32, HarnessError> {
    i32::try_from(value)
        .map_err(|_| HarnessError::invalid(format!("{label} exceeds PostgreSQL integer")))
}

fn from_i32(value: i32, label: &str) -> Result<u32, HarnessError> {
    u32::try_from(value).map_err(|_| HarnessError::execution(format!("{label} is negative")))
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn database_error(error: sqlx::Error) -> HarnessError {
    HarnessError::execution(format!("control database error: {error}"))
}

pub(crate) fn workspace_write_error(error: sqlx::Error) -> HarnessError {
    if error.as_database_error().is_some_and(|failure| {
        failure.is_unique_violation()
            && (failure.constraint() == Some("control_workspaces_registered_name")
                || failure
                    .message()
                    .contains("control_workspaces_registered_name")
                || failure.message().contains("control_workspaces.name"))
    }) {
        HarnessError::conflict("workspace name already exists in this project")
    } else {
        database_error(error)
    }
}

#[allow(clippy::needless_pass_by_value)]
fn json_error(error: serde_json::Error) -> HarnessError {
    HarnessError::execution(format!("control JSON error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_external_names_at_the_control_boundary() {
        assert!(validate_slug("team-a").is_ok());
        assert!(validate_slug("Team A").is_err());
        assert!(validate_secret_name("provider.api-key").is_ok());
        assert!(validate_secret_name("../api-key").is_err());
    }

    #[test]
    fn audit_payload_is_deterministic() {
        let first = audit_payload(
            "audit",
            "tenant",
            Some("user"),
            "user",
            "secret.rotate",
            "secret",
            "key",
            "success",
            &json!({ "version": 1 }),
            1,
        )
        .unwrap();
        let second = audit_payload(
            "audit",
            "tenant",
            Some("user"),
            "user",
            "secret.rotate",
            "secret",
            "key",
            "success",
            &json!({ "version": 1 }),
            1,
        )
        .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn audit_payload_is_stable_across_jsonb_key_reordering() {
        let first = audit_payload(
            "audit",
            "tenant",
            Some("user"),
            "user",
            "workspace.create",
            "workspace",
            "workspace-id",
            "success",
            &json!({ "z": 1, "a": { "y": 2, "b": 3 } }),
            1,
        )
        .unwrap();
        let second = audit_payload(
            "audit",
            "tenant",
            Some("user"),
            "user",
            "workspace.create",
            "workspace",
            "workspace-id",
            "success",
            &json!({ "a": { "b": 3, "y": 2 }, "z": 1 }),
            1,
        )
        .unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn extension_lifecycle_cleanup_removes_only_matching_package_mounts() {
        let mut entries = vec![
            PluginEntry {
                id: "extension:tools.example@1.0.0".to_owned(),
                kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
                enabled: true,
                config: json!({
                    "package_id": "tools.example",
                    "version": "1.0.0",
                    "settings": {}
                }),
            },
            PluginEntry {
                id: "extension:tools.example@2.0.0".to_owned(),
                kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
                enabled: true,
                config: json!({
                    "package_id": "tools.example",
                    "version": "2.0.0",
                    "settings": {}
                }),
            },
            PluginEntry {
                id: "prompt".to_owned(),
                kind: "ternilo.prompt.section".to_owned(),
                enabled: true,
                config: json!({}),
            },
        ];
        let packages = [("tools.example".to_owned(), "1.0.0".to_owned())]
            .into_iter()
            .collect();

        assert!(remove_extension_entries(&mut entries, &packages));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["extension:tools.example@2.0.0", "prompt"]
        );
        assert!(!remove_extension_entries(&mut entries, &packages));
    }

    #[test]
    fn cloud_extension_resolution_rejects_duplicate_mounts_tool_and_command_names_early() {
        let profile = ternilo_protocol::Profile {
            plugins: vec![
                PluginEntry {
                    id: "extension:first".to_owned(),
                    kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
                    enabled: true,
                    config: json!({
                        "package_id": "tools.example",
                        "version": "1.0.0",
                        "settings": { "prefix": "first" }
                    }),
                },
                PluginEntry {
                    id: "extension:second".to_owned(),
                    kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
                    enabled: true,
                    config: json!({
                        "package_id": "tools.example",
                        "version": "1.0.0",
                        "settings": { "prefix": "second" }
                    }),
                },
            ],
        };
        let duplicate_mount = ternilo_extension::unique_extension_mounts(&profile).unwrap_err();
        assert!(duplicate_mount.message.contains("mounted more than once"));

        let mut first = serde_json::from_str::<ternilo_extension::ExtensionManifest>(include_str!(
            "../../../examples/rhai-echo-extension/manifest.json"
        ))
        .unwrap();
        let mut second = first.clone();
        second.package_id = "dev.ternilo.rhai-echo-copy".to_owned();
        second.version = "2.0.0".to_owned();
        let duplicate_tool =
            ternilo_extension::validate_extension_tool_name_uniqueness([&first, &second])
                .unwrap_err();
        assert!(duplicate_tool.message.contains("rhai_echo"));
        assert!(
            duplicate_tool
                .message
                .contains("dev.ternilo.rhai-echo@1.0.0")
        );
        assert!(
            duplicate_tool
                .message
                .contains("dev.ternilo.rhai-echo-copy@2.0.0")
        );

        first.contributions.commands = vec![ternilo_extension::ExtensionCommandContribution {
            name: "shared-review".to_owned(),
            description: "Review through the first package".to_owned(),
            tool: "rhai_echo".to_owned(),
            input: None,
            fixed_arguments: json!({ "text": "first" }),
        }];
        second.contributions.commands = vec![ternilo_extension::ExtensionCommandContribution {
            name: "shared-review".to_owned(),
            description: "Review through the second package".to_owned(),
            tool: "rhai_echo".to_owned(),
            input: None,
            fixed_arguments: json!({ "text": "second" }),
        }];
        let duplicate_command =
            ternilo_extension::validate_extension_command_name_uniqueness([&first, &second])
                .unwrap_err();
        assert!(duplicate_command.message.contains("shared-review"));
        assert!(
            duplicate_command
                .message
                .contains("dev.ternilo.rhai-echo@1.0.0")
        );
        assert!(
            duplicate_command
                .message
                .contains("dev.ternilo.rhai-echo-copy@2.0.0")
        );
    }

    #[test]
    fn extension_version_identity_pins_the_signature_and_capability_grants() {
        let manifest = serde_json::from_str::<ternilo_extension::ExtensionManifest>(include_str!(
            "../../../examples/rhai-echo-extension/manifest.json"
        ))
        .unwrap();
        let request = ternilo_extension::ExtensionInstallRequest {
            bundle: ternilo_extension::SignedExtensionBundle {
                manifest,
                payload: ternilo_extension::ExtensionPayload::Utf8("fixture".to_owned()),
                signature_base64: "signature-a".to_owned(),
            },
            granted_capabilities: BTreeSet::new(),
        };
        let identity = extension_install_identity(&request);
        assert_eq!(identity, extension_install_identity(&request.clone()));
        validate_extension_version_identity(Some(&identity), &identity).unwrap();

        let mut different_signature = request.clone();
        different_signature.bundle.signature_base64 = "signature-b".to_owned();
        let different_signature = extension_install_identity(&different_signature);
        assert!(
            validate_extension_version_identity(Some(&identity), &different_signature).is_err()
        );

        let mut different_grants = request;
        different_grants
            .granted_capabilities
            .insert(ternilo_extension::Capability::Log);
        let different_grants = extension_install_identity(&different_grants);
        assert!(validate_extension_version_identity(Some(&identity), &different_grants).is_err());
    }
}
