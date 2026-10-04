#![forbid(unsafe_code)]

use std::{collections::BTreeSet, path::Path, sync::Arc, time::Duration};

use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Router, handler},
};
use salvo_extra::{affix_state, size_limiter::max_size};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use ternilo_cloud::{
    CloudRunDraft, CloudRunRecord, CloudSessionEventFeed, CloudStore, CompiledRun, WorkerPolicy,
};
use ternilo_control::{
    ControlStore, ControlUser, ModelUsageReport, SecretCipher, TenantQuota, TenantRole,
    WorkspacePlacement,
};
use ternilo_protocol::{
    AgentId, Attachment, ErrorCode, HarnessError, PermissionPreset, SessionId, TenantId, UserId,
    WorkspaceId,
};
use ternilo_transport::{ApplicationOperation, ExecutorId};
use zeroize::Zeroizing;

mod admin;
mod auth;
#[cfg(test)]
mod cloud_access_tests;
mod computer_models;
mod diagnostics;
mod edge;
mod execution_maintenance;
mod groups;
pub(crate) mod http;
mod identity;
mod live;
mod model_gateway;
mod models;
mod node_cleanup;
mod security;
mod service_accounts;
mod state;
pub(crate) mod web;
mod workbench;
mod worker_api;

use edge::EdgeGateway;
pub(crate) use edge::validate_cluster_origin;
use http::{ApiError, invalid_request, now_ms, path_parameter, tenant_parameter};
use state::{AppState, actor, app_state};

mod spaces;
use spaces::{
    create_project, create_tenant, create_workspace, delete_project, get_workspace,
    list_memberships, list_projects, list_tenants, list_workspaces, remove_membership,
    rename_project, set_membership,
};
mod computers;
use computers::{
    consume_enrollment, create_enrollment, create_owned_enrollment, get_managed_computer,
    get_owned_computer, list_executors, list_owned_executors, recover_managed_computer,
    recover_owned_computer, remove_managed_computer, remove_owned_computer, revoke_executor,
    revoke_owned_executor, suspend_managed_computer, suspend_owned_computer,
    update_managed_computer, update_owned_computer,
};
mod tenant_services;
use tenant_services::{
    delete_secret, get_model_usage, get_quota, list_audit, list_secrets, put_secret, release_quota,
    reserve_quota, update_quota,
};
mod extensions;
use extensions::{
    install_extension, list_extensions, revoke_extension, revoke_extension_publisher,
    set_extension_state, trust_extension_publisher, uninstall_extension,
};
mod execution;
use execution::{
    cancel_cloud_run, cloud_session_events, get_cloud_run, list_cloud_runs, list_cloud_sessions,
    submit_cloud_chat, submit_cloud_run,
};
mod capabilities;
use capabilities::{workbench_catalog, workbench_plugins};
pub(crate) use execution::require_managed_execution;

const MAX_API_BODY_BYTES: u64 = 24 * 1024 * 1024;

pub(crate) struct Runtime {
    pub router: Arc<Router>,
    pub store: ControlStore,
    shutdown: tokio::sync::watch::Sender<bool>,
    edge: Arc<EdgeGateway>,
    maintenance: tokio::task::JoinHandle<()>,
}

impl Runtime {
    pub async fn shutdown(&self) {
        self.shutdown.send_replace(true);
        self.edge.shutdown().await;
        self.maintenance.abort();
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        self.maintenance.abort();
    }
}

pub(crate) async fn execute(config: crate::config::ServerConfig) -> Result<(), HarnessError> {
    let listen = config.listen;
    let runtime = prepare(config).await?;
    crate::http::serve(
        listen,
        Arc::clone(&runtime.router),
        "server",
        runtime.shutdown(),
    )
    .await
}

pub(crate) async fn prepare(
    mut config: crate::config::ServerConfig,
) -> Result<Runtime, HarnessError> {
    config.validate()?;
    let encoded_key = Zeroizing::new(config.secret_master_key.clone());
    let cipher = SecretCipher::from_base64(&encoded_key)?;
    let store = ControlStore::connect(
        &config.database_url,
        config.migration_database_url.as_deref(),
        cipher,
        config.max_database_connections,
    )
    .await?;
    // Install every final schema before the restricted runtime pool serves requests.
    if let Some(url) = &config.migration_database_url {
        let database = ternilo_storage::Database::connect(url, 1).await?;
        CloudStore::from_database(database.clone()).await?;
        crate::gateway_journal::GatewayJournal::open(database.clone()).await?;
        diagnostics::initialize(&database).await?;
        database.close().await;
    }
    let cloud = CloudStore::from_database(store.database().clone()).await?;
    let cloud_events = CloudSessionEventFeed::from_database(store.database().clone()).await?;
    let mut edge = EdgeGateway::new(store.edge_store()).await?;
    if let Some(origin) = &config.cluster_url {
        edge = edge.with_cluster(origin, &encoded_key).await?;
    }
    let edge = Arc::new(edge);
    diagnostics::initialize(store.database()).await?;
    let catalog = ternilo_cloud::catalog()?;
    let worker_policy = load_worker_policy(config.worker_policy.as_deref(), &catalog)?;
    let security = Arc::new(security::SecurityState::from_config(&config));
    if store.instance_settings().await?.is_none() && config.setup_token_hash.is_none() {
        let token = crate::bootstrap::new_setup_token();
        println!("Initialization Key: {token}");
        config.setup_token_hash = Some(crate::config::digest_token(&token));
    }
    let runtime_store = store.clone();
    let runtime_edge = Arc::clone(&edge);
    let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
    let model_maintenance = models::start_maintenance(store.clone(), shutdown_receiver.clone());
    let app = web_router(AppState {
        store,
        cloud,
        cloud_events,
        edge,
        security,
        worker_policy: Arc::new(worker_policy),
        catalog: Arc::new(catalog),
        managed_execution_enabled: config.managed_execution_enabled,
        setup_token_hash: config.setup_token_hash.clone(),
        shutdown: shutdown_receiver,
    });
    Ok(Runtime {
        router: Arc::new(app),
        store: runtime_store,
        shutdown,
        edge: runtime_edge,
        maintenance: model_maintenance,
    })
}

fn load_worker_policy(
    path: Option<&Path>,
    catalog: &ternilo_kernel::Catalog,
) -> Result<WorkerPolicy, HarnessError> {
    let policy = if let Some(path) = path {
        read_json(path)?
    } else {
        WorkerPolicy {
            catalog_revision: catalog.revision().to_owned(),
            policy_revision: "server-default-v1".to_owned(),
            maximum_limits: ternilo_protocol::RunLimits::default(),
            max_run_attempts: 2,
            max_tenant_workspace_bytes: 10 * 1024 * 1024 * 1024,
            max_tenant_workspace_entries: 100_000,
            minimum_workspace_free_bytes: 1024 * 1024 * 1024,
            allowed_plugin_kinds: catalog
                .kinds()
                .filter(|kind| *kind != "ternilo.model.openai_compatible")
                .map(str::to_owned)
                .collect(),
            max_extension_packages_per_run: 4,
            extension_host_policy: ternilo_extension::ExtensionHostPolicy::default(),
            denied_tools: BTreeSet::default(),
        }
    };
    policy.validate_operational_limits()?;
    if policy.catalog_revision != catalog.revision() {
        return Err(HarnessError::policy(format!(
            "worker policy catalog {} does not match linked catalog {}",
            policy.catalog_revision,
            catalog.revision(),
        )));
    }
    Ok(policy)
}

fn web_router(state: AppState) -> Router {
    let diagnostics = diagnostics::router(state.store.database().clone(), Arc::clone(&state.edge));
    Router::new()
        .hoop(affix_state::inject(state))
        .push(diagnostics)
        .push(web::router())
        .push(identity::router())
        .push(admin::router())
        .push(models::administration_router())
        .push(models::access_router())
        .push(models::gateway_router())
        .push(models::device_router())
        .push(execution_maintenance::router())
        .push(worker_api::router())
        .push(worker_api::management_router())
        .push(model_gateway::router())
        .push(model_gateway::node_router())
        .push(node_cleanup::router())
        .push(Router::with_path("health").get(health))
        .push(api_router())
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep the platform API route tree visible in one place."
)]
fn api_router() -> Router {
    let quota = Router::with_path("quota")
        .get(get_quota)
        .put(update_quota)
        .push(
            Router::with_path("reservations").post(reserve_quota).push(
                Router::with_path("{reservation_id}")
                    .push(Router::with_path("release").post(release_quota)),
            ),
        );
    let tenant = Router::with_path("{tenant_id}")
        .push(
            Router::with_path("projects")
                .get(list_projects)
                .post(create_project)
                .push(
                    Router::with_path("{project_id}")
                        .patch(rename_project)
                        .delete(delete_project)
                        .push(workbench::sharing::router()),
                ),
        )
        .push(
            Router::with_path("workspaces")
                .get(list_workspaces)
                .post(create_workspace)
                .push(Router::with_path("{workspace_id}").get(get_workspace)),
        )
        .push(
            Router::with_path("members").get(list_memberships).push(
                Router::with_path("{user_id}")
                    .put(set_membership)
                    .delete(remove_membership),
            ),
        )
        .push(groups::router())
        .push(service_accounts::router())
        .push(
            Router::with_path("executors").get(list_executors).push(
                Router::with_path("{executor_id}")
                    .get(get_managed_computer)
                    .patch(update_managed_computer)
                    .delete(revoke_executor)
                    .push(Router::with_path("suspension").put(suspend_managed_computer))
                    .push(Router::with_path("recovery").post(recover_managed_computer))
                    .push(Router::with_path("registration").delete(remove_managed_computer)),
            ),
        )
        .push(
            Router::with_path("my-computers")
                .get(list_owned_executors)
                .push(
                    Router::with_path("{executor_id}")
                        .get(get_owned_computer)
                        .patch(update_owned_computer)
                        .delete(revoke_owned_executor)
                        .push(Router::with_path("suspension").put(suspend_owned_computer))
                        .push(Router::with_path("recovery").post(recover_owned_computer))
                        .push(Router::with_path("registration").delete(remove_owned_computer)),
                ),
        )
        .push(Router::with_path("enrollments").post(create_enrollment))
        .push(Router::with_path("my-computer-enrollments").post(create_owned_enrollment))
        .push(quota)
        .push(Router::with_path("model-usage").get(get_model_usage))
        .push(
            Router::with_path("secrets")
                .get(list_secrets)
                .post(put_secret)
                .push(Router::with_path("{name}").delete(delete_secret)),
        )
        .push(Router::with_path("audit").get(list_audit))
        .push(
            Router::with_path("runs")
                .get(list_cloud_runs)
                .post(submit_cloud_run)
                .push(Router::with_path("chat").post(submit_cloud_chat))
                .push(
                    Router::with_path("{run_id}")
                        .get(get_cloud_run)
                        .delete(cancel_cloud_run),
                ),
        )
        .push(
            Router::with_path("extensions")
                .get(list_extensions)
                .post(install_extension)
                .push(
                    Router::with_path("publishers")
                        .post(trust_extension_publisher)
                        .push(
                            Router::with_path("{key_id}/revoke").post(revoke_extension_publisher),
                        ),
                )
                .push(
                    Router::with_path("{package_id}/{version}")
                        .put(set_extension_state)
                        .delete(uninstall_extension)
                        .push(Router::with_path("revoke").post(revoke_extension)),
                ),
        )
        .push(
            Router::with_path("sessions")
                .get(list_cloud_sessions)
                .push(Router::with_path("{session_id}/events").get(cloud_session_events)),
        );
    let authenticated = Router::new()
        .hoop(auth::user_auth)
        .push(Router::with_path("me").get(state::me))
        .push(Router::with_path("cloud-config").get(cloud_configuration))
        .push(workbench_router())
        .push(
            Router::with_path("tenants")
                .get(list_tenants)
                .post(create_tenant)
                .push(tenant),
        );
    Router::with_path("api/v1")
        .hoop(max_size(MAX_API_BODY_BYTES))
        .push(Router::with_path("live").get(live::upgrade))
        .push(Router::with_path("enrollments/consume").post(consume_enrollment))
        .push(edge::router())
        .push(edge::peer_router())
        .push(authenticated)
}

fn workbench_router() -> Router {
    workbench::router()
}

fn decode_node<T: DeserializeOwned>(value: Value, label: &str) -> Result<T, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::execution(format!("decode {label}: {error}")))
}

fn encode_node(value: &impl Serialize, label: &str) -> Result<Value, HarnessError> {
    serde_json::to_value(value)
        .map_err(|error| HarnessError::execution(format!("encode {label}: {error}")))
}

#[handler]
async fn health(depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    app_state(depot)
        .store
        .health()
        .await
        .map_err(ApiError::unavailable)?;
    app_state(depot)
        .cloud
        .health()
        .await
        .map_err(ApiError::unavailable)?;
    app_state(depot)
        .edge
        .health()
        .await
        .map_err(ApiError::unavailable)?;
    Ok(Json(json!({ "status": "ok" })))
}

#[handler]
fn cloud_configuration(depot: &mut Depot) -> Json<Value> {
    let policy = &app_state(depot).worker_policy;
    Json(json!({
        "managed_execution_enabled": app_state(depot).managed_execution_enabled,
        "catalog_revision": policy.catalog_revision,
        "policy_revision": policy.policy_revision,
        "maximum_limits": policy.maximum_limits,
    }))
}

fn read_json<T>(path: &Path) -> Result<T, HarnessError>
where
    T: serde::de::DeserializeOwned,
{
    let text = std::fs::read_to_string(path)
        .map_err(|error| HarnessError::invalid(format!("read {}: {error}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|error| HarnessError::invalid(format!("parse {}: {error}", path.display())))
}

#[cfg(test)]
mod tests;
