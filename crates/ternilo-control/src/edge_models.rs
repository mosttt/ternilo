use crate::{
    ControlStore, ControlUser, EdgeSessionMetadata, EdgeStore, NodeModelPrincipal, NodePrincipal,
    ResourceAction, ResourceKind, resource_access_in,
};
use sqlx::Row;
use ternilo_protocol::{
    HarnessError, InputAuthor, NodeModelRequest, RunModelSnapshot, SessionId, TenantId,
};
use ternilo_storage::{Json, Transaction, database_error, for_update, set_tenant_scope};

mod schedules;

impl ControlStore {
    pub async fn node_model_session_registered(
        &self,
        node: &NodePrincipal,
        session: &SessionId,
    ) -> Result<bool, HarnessError> {
        let mut transaction = self
            .database
            .tenant_transaction(&node.scope.tenant_id)
            .await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_edge_sessions WHERE tenant_id=$1 AND executor_id=$2 AND node_session_id=$3")
            .bind(node.scope.tenant_id.as_str()).bind(node.executor_id.as_str()).bind(session.as_str())
            .fetch_one(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(count == 1)
    }

    pub async fn account_provider_space(
        &self,
        owner: &ternilo_protocol::UserId,
    ) -> Result<TenantId, HarnessError> {
        let mut tx = self.database.begin().await?;
        let tenant = Self::account_provider_space_in(&mut tx, owner).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(tenant)
    }

    pub async fn account_provider_space_in(
        tx: &mut Transaction,
        owner: &ternilo_protocol::UserId,
    ) -> Result<TenantId, HarnessError> {
        crate::account_store::personal_space_in(tx, owner)
            .await
            .map(|(tenant, _)| tenant)
    }

    pub async fn require_edge_model_owner_access(
        &self,
        owner: &ternilo_protocol::UserId,
        tenant: &TenantId,
        session: &SessionId,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.begin().await?;
        require_model_resource_action(&mut tx, owner, tenant, session, ResourceAction::Configure)
            .await?;
        tx.commit().await.map_err(database_error)
    }

    /// Only trusted Server configuration writes the account model authorization.
    pub async fn set_edge_session_model_snapshot(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        session: &SessionId,
        snapshot: Option<RunModelSnapshot>,
    ) -> Result<(), HarnessError> {
        if let Some(snapshot) = &snapshot {
            snapshot.validate()?;
        }
        let mut tx = self.database.begin().await?;
        set_tenant_scope(&mut tx, tenant).await?;
        resource_access_in(
            &mut tx,
            &actor.user_id,
            tenant,
            ResourceKind::Session,
            session.as_str(),
        )
        .await?
        .require(ResourceAction::Configure)?;
        let mut metadata: Json<EdgeSessionMetadata> = sqlx::query_scalar(for_update(&tx,
            "SELECT metadata_json FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2",
            "SELECT metadata_json FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE"))
            .bind(tenant.as_str()).bind(session.as_str()).fetch_one(&mut *tx).await.map_err(database_error)?;
        metadata.0.server_model = snapshot;
        sqlx::query("UPDATE control_edge_sessions SET metadata_json=$3 WHERE tenant_id=$1 AND session_id=$2")
            .bind(tenant.as_str()).bind(session.as_str()).bind(metadata).execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)
    }

    /// Resolve account and resource authority without accepting identity claims from the Node.
    pub async fn authorize_node_model_in(
        &self,
        tx: &mut Transaction,
        node: &NodePrincipal,
        body: &NodeModelRequest,
    ) -> Result<NodeModelPrincipal, HarnessError> {
        body.session_id.validate()?;
        body.origin_session_id.validate()?;
        body.run_id.validate()?;
        body.binding.validate()?;
        if body.request.run_id != body.run_id {
            return Err(HarnessError::policy(
                "model request run differs from its authorized input",
            ));
        }
        let tenant = &node.scope.tenant_id;
        set_tenant_scope(tx, tenant).await?;
        let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_node_credentials c JOIN control_executors e ON e.tenant_id=c.tenant_id AND e.executor_id=c.executor_id WHERE c.tenant_id=$1 AND c.credential_id=$2 AND c.executor_id=$3 AND c.revoked_at_ms IS NULL AND e.state<>'revoked' AND e.owner_user_id=$4")
            .bind(tenant.as_str()).bind(&node.credential_id).bind(node.executor_id.as_str()).bind(node.scope.user_id.as_str())
            .fetch_one(&mut **tx).await.map_err(database_error)?;
        if active != 1 {
            return Err(HarnessError::policy("Node model credential was revoked"));
        }
        crate::account_store::require_active_account_in(tx, &node.scope.user_id).await?;
        set_tenant_scope(tx, tenant).await?;
        let row = sqlx::query("SELECT session_id, owner_user_id, metadata_json FROM control_edge_sessions WHERE tenant_id=$1 AND executor_id=$2 AND node_session_id=$3")
            .bind(tenant.as_str()).bind(node.executor_id.as_str()).bind(body.session_id.as_str())
            .fetch_optional(&mut **tx).await.map_err(database_error)?
            .ok_or_else(|| HarnessError::policy("Node model session is not registered on this Server"))?;
        let metadata: Json<EdgeSessionMetadata> =
            row.try_get("metadata_json").map_err(database_error)?;
        let snapshot = metadata.0.server_model.ok_or_else(|| {
            HarnessError::policy("this session has no account Provider authorization")
        })?;
        if snapshot.binding != body.binding {
            return Err(HarnessError::policy(
                "Node requested a model outside this session's account Provider",
            ));
        }
        if matches!(
            &snapshot.binding,
            ternilo_protocol::RunModelBinding::ComputerProvider { .. }
        ) && !matches!(
            body.provenance.as_ref().map(|input| &input.author),
            Some(InputAuthor::Account { .. })
        ) {
            return Err(HarnessError::policy(
                "computer models require input submitted through Server",
            ));
        }
        let owner = snapshot.binding.beneficiary_user_id();
        let origin_run = schedules::verify(tx, node, body).await?;
        let actor = model_input_actor(tx, node, body, owner, &origin_run).await?;
        let session_id = SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        );
        let origin: String = sqlx::query_scalar("SELECT session_id FROM control_edge_sessions WHERE tenant_id=$1 AND executor_id=$2 AND node_session_id=$3")
            .bind(tenant.as_str()).bind(node.executor_id.as_str()).bind(body.origin_session_id.as_str())
            .fetch_one(&mut **tx).await.map_err(database_error)?;
        require_model_resource_action(
            tx,
            &actor,
            tenant,
            &SessionId::new(origin),
            ResourceAction::Submit,
        )
        .await?;
        require_model_resource_action(tx, owner, tenant, &session_id, ResourceAction::Configure)
            .await?;
        Ok(NodeModelPrincipal {
            credential_id: node.credential_id.clone(),
            tenant_id: tenant.clone(),
            session_id,
            run_id: body.run_id.clone(),
            actor_user_id: actor,
            resource_owner_user_id: ternilo_protocol::UserId::new(
                row.try_get::<String, _>("owner_user_id")
                    .map_err(database_error)?,
            ),
            snapshot,
        })
    }
}

async fn model_input_actor(
    tx: &mut Transaction,
    node: &NodePrincipal,
    body: &NodeModelRequest,
    owner: &ternilo_protocol::UserId,
    origin_run: &ternilo_protocol::RunId,
) -> Result<ternilo_protocol::UserId, HarnessError> {
    let tenant = &node.scope.tenant_id;
    match body.provenance.as_ref().map(|value| &value.author) {
        Some(InputAuthor::Account { user_id, .. }) => {
            let provenance = body.provenance.as_ref().expect("account input");
            if provenance.run_id.as_ref() != Some(origin_run) {
                return Err(HarnessError::policy(
                    "account model input is not bound to this run; submit a new task",
                ));
            }
            EdgeStore::verify_account_model_input_in_transaction(
                tx,
                tenant,
                &node.executor_id,
                &body.origin_session_id,
                provenance,
            )
            .await?;
            let accepted_origin: String = sqlx::query_scalar("SELECT session_id FROM control_edge_input_provenance WHERE tenant_id=$1 AND executor_id=$2 AND input_id=$3")
                .bind(tenant.as_str()).bind(node.executor_id.as_str()).bind(provenance.input_id.as_str())
                .fetch_one(&mut **tx).await.map_err(database_error)?;
            require_model_lineage(
                tx,
                node,
                &body.origin_session_id,
                &SessionId::new(accepted_origin),
            )
            .await?;
            Ok(user_id.clone())
        }
        None | Some(InputAuthor::Local) if owner == &node.scope.user_id => {
            Ok(node.scope.user_id.clone())
        }
        _ => Err(HarnessError::policy(
            "model input has no authorized account origin",
        )),
    }
}

async fn require_model_resource_action(
    tx: &mut Transaction,
    user: &ternilo_protocol::UserId,
    tenant: &TenantId,
    session: &SessionId,
    action: ResourceAction,
) -> Result<(), HarnessError> {
    let mut current = session.clone();
    let mut visited = std::collections::BTreeSet::new();
    while visited.insert(current.clone()) {
        let access =
            resource_access_in(tx, user, tenant, ResourceKind::Session, current.as_str()).await?;
        if access.require(action).is_ok() {
            return Ok(());
        }
        let parent: Option<String> = sqlx::query_scalar("SELECT parent.session_id FROM control_edge_sessions child JOIN control_edge_session_provenance lineage ON lineage.tenant_id=child.tenant_id AND lineage.executor_id=child.executor_id AND lineage.session_id=child.node_session_id JOIN control_edge_sessions parent ON parent.tenant_id=child.tenant_id AND parent.executor_id=child.executor_id AND parent.node_session_id=lineage.parent_session_id WHERE child.tenant_id=$1 AND child.session_id=$2 AND lineage.subagent_id IS NOT NULL")
            .bind(tenant.as_str()).bind(current.as_str()).fetch_optional(&mut **tx).await.map_err(database_error)?;
        let Some(parent) = parent else {
            break;
        };
        current = SessionId::new(parent);
    }
    Err(HarnessError::policy(
        "model access requires permission on this session or its originating subagent task",
    ))
}

async fn require_model_lineage(
    tx: &mut Transaction,
    node: &NodePrincipal,
    session: &SessionId,
    origin: &SessionId,
) -> Result<(), HarnessError> {
    let mut current = session.as_str().to_owned();
    let mut visited = std::collections::BTreeSet::new();
    while visited.insert(current.clone()) {
        if current == origin.as_str() {
            return Ok(());
        }
        let parent: Option<String> = sqlx::query_scalar(
            "SELECT parent_session_id FROM control_edge_session_provenance WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3 AND subagent_id IS NOT NULL",
        ).bind(node.scope.tenant_id.as_str()).bind(node.executor_id.as_str()).bind(&current)
            .fetch_optional(&mut **tx).await.map_err(database_error)?.flatten();
        let Some(parent) = parent else {
            break;
        };
        current = parent;
    }
    Err(HarnessError::policy(
        "model input origin is not this session or its subagent ancestor",
    ))
}
