use super::{
    Database, GatewayJournal, GatewayLease, HarnessError, RouteKey, Row, database_error, integer,
    require_lease,
};
use ternilo_protocol::TenantId;
use ternilo_storage::Transaction;
use ternilo_transport::ExecutorId;

pub(super) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "gateway_cluster",
            1,
            include_str!("cluster_schema.sql"),
            include_str!("cluster_access.sql"),
        )
        .await
}

pub(crate) struct PeerRoute {
    pub(crate) lease: GatewayLease,
    pub(crate) endpoint: String,
    pub(crate) principal: ternilo_control::NodePrincipal,
}

pub(crate) struct LiveRoute {
    pub(crate) route: RouteKey,
    pub(crate) owner_id: String,
    pub(crate) revision: i64,
    pub(crate) expires_at_ms: i64,
}

pub(super) async fn acquired(
    tx: &mut Transaction,
    route: &RouteKey,
    lease: &GatewayLease,
    expires: u64,
) -> Result<(), HarnessError> {
    sqlx::query(
        "INSERT INTO gateway_live_routes
        (tenant_id,executor_id,owner_id,fencing_token,revision,expires_at_ms)
        VALUES ($1,$2,$3,$4,1,$5) ON CONFLICT (tenant_id,executor_id) DO UPDATE SET
        owner_id=EXCLUDED.owner_id,fencing_token=EXCLUDED.fencing_token,
        revision=gateway_live_routes.revision+1,expires_at_ms=EXCLUDED.expires_at_ms",
    )
    .bind(route.tenant_id.as_str())
    .bind(route.executor_id.as_str())
    .bind(&lease.owner_id)
    .bind(integer(lease.fencing_token)?)
    .bind(integer(expires)?)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub(super) async fn renewed(
    tx: &mut Transaction,
    route: &RouteKey,
    lease: &GatewayLease,
    expires: u64,
) -> Result<(), HarnessError> {
    sqlx::query(
        "UPDATE gateway_live_routes SET expires_at_ms=$5
        WHERE tenant_id=$1 AND executor_id=$2 AND owner_id=$3 AND fencing_token=$4",
    )
    .bind(route.tenant_id.as_str())
    .bind(route.executor_id.as_str())
    .bind(&lease.owner_id)
    .bind(integer(lease.fencing_token)?)
    .bind(integer(expires)?)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub(super) async fn changed(tx: &mut Transaction, route: &RouteKey) -> Result<(), HarnessError> {
    sqlx::query(
        "UPDATE gateway_live_routes SET revision=revision+1 WHERE tenant_id=$1 AND executor_id=$2",
    )
    .bind(route.tenant_id.as_str())
    .bind(route.executor_id.as_str())
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

impl GatewayJournal {
    pub(crate) async fn leased_executor_ids(
        &self,
        tenant: &TenantId,
        now: u64,
    ) -> Result<Vec<ExecutorId>, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        let ids = sqlx::query_scalar::<_, String>(
            "SELECT executor_id FROM gateway_leases WHERE tenant_id=$1 AND expires_at_ms>$2",
        )
        .bind(tenant.as_str())
        .bind(integer(now)?)
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(ids.into_iter().map(ExecutorId::new).collect())
    }
    pub(crate) async fn publish_peer(
        &self,
        route: &RouteKey,
        lease: &GatewayLease,
        endpoint: &str,
        principal: &ternilo_control::NodePrincipal,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.transaction(route).await?;
        require_lease(&mut tx, route, lease, now).await?;
        sqlx::query("INSERT INTO gateway_peer_routes (tenant_id,executor_id,owner_id,fencing_token,endpoint,principal_json)
            VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (tenant_id,executor_id) DO UPDATE SET
            owner_id=EXCLUDED.owner_id,fencing_token=EXCLUDED.fencing_token,endpoint=EXCLUDED.endpoint,principal_json=EXCLUDED.principal_json")
            .bind(route.tenant_id.as_str()).bind(route.executor_id.as_str()).bind(&lease.owner_id)
            .bind(integer(lease.fencing_token)?).bind(endpoint)
            .bind(ternilo_storage::Json(principal))
            .execute(&mut *tx).await.map_err(database_error)?;
        changed(&mut tx, route).await?;
        tx.commit().await.map_err(database_error)
    }

    pub(crate) async fn peer_route(
        &self,
        route: &RouteKey,
        now: u64,
    ) -> Result<Option<PeerRoute>, HarnessError> {
        let mut tx = self.database.tenant_transaction(&route.tenant_id).await?;
        let row = sqlx::query(
            "SELECT p.owner_id,p.fencing_token,p.endpoint,p.principal_json FROM gateway_peer_routes p
            JOIN gateway_leases l ON l.tenant_id=p.tenant_id AND l.executor_id=p.executor_id
            AND l.owner_id=p.owner_id AND l.fencing_token=p.fencing_token
            WHERE p.tenant_id=$1 AND p.executor_id=$2 AND l.expires_at_ms>$3",
        )
        .bind(route.tenant_id.as_str())
        .bind(route.executor_id.as_str())
        .bind(integer(now)?)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        row.map(|row| {
            Ok(PeerRoute {
                lease: GatewayLease {
                    owner_id: row.try_get("owner_id").map_err(database_error)?,
                    fencing_token: super::unsigned(
                        row.try_get("fencing_token").map_err(database_error)?,
                    )?,
                },
                endpoint: row.try_get("endpoint").map_err(database_error)?,
                principal: row
                    .try_get::<ternilo_storage::Json<ternilo_control::NodePrincipal>, _>(
                        "principal_json",
                    )
                    .map_err(database_error)?
                    .0,
            })
        })
        .transpose()
    }

    pub(crate) async fn check_lease(
        &self,
        route: &RouteKey,
        lease: &GatewayLease,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.tenant_transaction(&route.tenant_id).await?;
        require_lease(&mut tx, route, lease, now).await?;
        tx.commit().await.map_err(database_error)
    }

    pub(crate) async fn notify_live(
        &self,
        route: &RouteKey,
        lease: &GatewayLease,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.transaction(route).await?;
        require_lease(&mut tx, route, lease, now).await?;
        changed(&mut tx, route).await?;
        tx.commit().await.map_err(database_error)
    }

    pub(crate) async fn live_routes(&self) -> Result<Vec<LiveRoute>, HarnessError> {
        sqlx::query(
            "SELECT tenant_id,executor_id,owner_id,revision,expires_at_ms FROM gateway_live_routes",
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|row| {
            Ok(LiveRoute {
                route: RouteKey::new(
                    TenantId::new(
                        row.try_get::<String, _>("tenant_id")
                            .map_err(database_error)?,
                    ),
                    ExecutorId::new(
                        row.try_get::<String, _>("executor_id")
                            .map_err(database_error)?,
                    ),
                ),
                owner_id: row.try_get("owner_id").map_err(database_error)?,
                revision: row.try_get("revision").map_err(database_error)?,
                expires_at_ms: row.try_get("expires_at_ms").map_err(database_error)?,
            })
        })
        .collect()
    }
}
