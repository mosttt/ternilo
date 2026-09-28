use std::{collections::BTreeSet, fmt::Write as _, net::SocketAddr, path::PathBuf, sync::Arc};

use rand::random;
use salvo_core::{
    conn::tcp::TcpAcceptor,
    http::{StatusCode, header},
    prelude::{Depot, FlowCtrl, Json, Request, Response, Router, Scribe, Server, Text, handler},
};
use salvo_extra::{affix_state, size_limiter::max_size};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ternilo_local::{
    DirectoryListing, LocalApplication, LocalSession, LocalSessionUpdate, LocalStateSnapshot,
    ModelSelection, PendingQuestion, SessionExport, Workspace,
};
use ternilo_protocol::{
    AgentPresetCopyRequest, AgentPresetDocument, AgentPresetRoster, AgentPresetUpdateRequest,
    ApplicationCatalog, Attachment, AuthorizationAttempt, AuthorizationBeginRequest,
    AuthorizationCredentialKey, AuthorizationPromptAnswer, AuthorizationSnapshot,
    CredentialInventory, CredentialRecordInfo, ExtensionProviderMaterializeRequest, FeedbackRating,
    HarnessError, PermissionPreset, PluginEntry, Profile, ProviderModelDiscoveryRequest,
    ProviderProfile, ReferenceCandidateRequest, ReferenceCandidateSnapshot, RunLimits, RunOutcome,
    SessionEvent, SessionMode, SessionProjectionSnapshot, SessionSearchHit, SessionSearchRequest,
    SessionStats, SessionTelemetrySharingStatus, SidebarOrdering, SubagentId, SubagentSnapshot,
    UserAnswer, WorkspaceId,
};

mod agent_team;
mod assets;
mod live;
mod model_connections;
mod presets;
mod providers;
mod session_queue;
mod sessions;
use sessions::{answer_question, pending_questions, search_sessions, session_router};
mod workspaces;
use assets::{
    css, icon, icon_192, icon_512, icon_maskable_512, index, javascript, offline_boot,
    offline_shell, service_worker, web_manifest,
};
use presets::{
    copy_agent_preset, delete_agent_preset, extension_router, get_agent_preset, list_agent_presets,
    set_default_agent_preset, update_agent_preset,
};
use providers::{
    answer_authorization_prompt, authorization_snapshot, begin_authorization, cancel_authorization,
    delete_credential_record, delete_provider, discover_provider_models, get_default_model,
    list_credentials, list_providers, materialize_extension_provider, remove_credential,
    set_credential, set_credential_record, set_default_model, upsert_provider,
};
use workspaces::{
    create_workspace, directory_listing, file_inventory, list_workspaces, make_directory,
    reference_candidates, rename_workspace, resolve_attachment, session_file_content,
    unregister_workspace, workspace_browser, workspace_browser_info,
};

const MAX_API_BODY_BYTES: u64 = 24 * 1024 * 1024;

#[derive(Clone)]
struct AppState {
    shared: Arc<Shared>,
}

struct Shared {
    application: Arc<LocalApplication>,
    api_token: String,
    accepted_hosts: BTreeSet<String>,
    service: Option<crate::service::ServiceControl>,
}

struct ApiError(HarnessError);

enum TurnResponse {
    Completed(RunOutcome),
    Cancelled,
}

impl Scribe for TurnResponse {
    fn render(self, response: &mut Response) {
        match self {
            Self::Completed(outcome) => response.render(Json(outcome)),
            Self::Cancelled => {
                response.status_code(StatusCode::NO_CONTENT);
            }
        }
    }
}

impl From<HarnessError> for ApiError {
    fn from(error: HarnessError) -> Self {
        Self(error)
    }
}

impl Scribe for ApiError {
    fn render(self, response: &mut Response) {
        let status = match self.0.code {
            ternilo_protocol::ErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
            ternilo_protocol::ErrorCode::Composition => StatusCode::UNPROCESSABLE_ENTITY,
            ternilo_protocol::ErrorCode::PolicyDenied => StatusCode::FORBIDDEN,
            ternilo_protocol::ErrorCode::Execution => StatusCode::INTERNAL_SERVER_ERROR,
            ternilo_protocol::ErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ternilo_protocol::ErrorCode::Cancelled | ternilo_protocol::ErrorCode::Conflict => {
                StatusCode::CONFLICT
            }
        };
        response.status_code(status);
        response.render(Json(json!({ "error": self.0 })));
    }
}

pub async fn serve(
    listen: SocketAddr,
    profile: Profile,
    data_dir: PathBuf,
) -> Result<(), HarnessError> {
    serve_with_limits(
        listen,
        profile,
        data_dir,
        RunLimits {
            max_tool_calls: 0,
            ..RunLimits::default()
        },
    )
    .await
}

/// Serve the local browser client with explicit per-turn run limits.
pub async fn serve_with_limits(
    listen: SocketAddr,
    profile: Profile,
    data_dir: PathBuf,
    limits: RunLimits,
) -> Result<(), HarnessError> {
    let application = crate::open_local_application_with_limits(profile, data_dir, limits).await?;
    let result = tokio::select! {
        result = serve_application(listen, Arc::clone(&application)) => result,
        () = shutdown_signal() => Ok(()),
    };
    let shutdown = application.shutdown().await;
    result?;
    shutdown
}

pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            signal = tokio::signal::ctrl_c() => signal.expect("install Ctrl-C handler"),
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("install Ctrl-C handler");
}

/// Serve the loopback browser client for an already-open application.
///
/// The caller owns application shutdown and may run other transports against
/// the same instance. Dropping this future stops accepting local HTTP traffic.
pub async fn serve_application(
    listen: SocketAddr,
    application: Arc<LocalApplication>,
) -> Result<(), HarnessError> {
    let listener = bind_loopback(listen).await?;
    serve_application_on_listener(listener, application).await
}

/// Bind a loopback listener before a native shell creates a privileged `WebView`.
pub async fn bind_loopback(listen: SocketAddr) -> Result<tokio::net::TcpListener, HarnessError> {
    if !listen.ip().is_loopback() {
        return Err(HarnessError::policy(
            "local web mode only binds to a loopback address",
        ));
    }
    tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| HarnessError::execution(format!("bind {listen}: {error}")))
}

/// Serve an already-bound loopback listener.
///
/// Pre-binding closes the race where another local process could occupy the
/// desktop shell's trusted origin between application startup and `WebView` load.
pub async fn serve_application_on_listener(
    listener: tokio::net::TcpListener,
    application: Arc<LocalApplication>,
) -> Result<(), HarnessError> {
    serve_managed(listener, application, random_api_token(), None).await
}

pub(crate) fn random_api_token() -> String {
    let mut token = String::with_capacity(64);
    for byte in random::<[u8; 32]>() {
        write!(&mut token, "{byte:02x}").expect("writing to String cannot fail");
    }
    token
}

pub(crate) async fn serve_managed(
    listener: tokio::net::TcpListener,
    application: Arc<LocalApplication>,
    api_token: String,
    service: Option<crate::service::ServiceControl>,
) -> Result<(), HarnessError> {
    application.resume_pending_submissions().await?;
    let address = listener
        .local_addr()
        .map_err(|error| HarnessError::execution(format!("read listen address: {error}")))?;
    if !address.ip().is_loopback() {
        return Err(HarnessError::policy(
            "local web mode only serves a loopback listener",
        ));
    }
    let accepted_hosts = accepted_hosts(address);
    let browser_enabled = service
        .as_ref()
        .is_none_or(|service| service.info.browser_enabled);
    let http_shutdown = service
        .as_ref()
        .map(|service| service.http_shutdown.clone());
    let state = AppState {
        shared: Arc::new(Shared {
            application,
            api_token,
            accepted_hosts,
            service,
        }),
    };
    let app = Router::new()
        .hoop(affix_state::inject(state))
        .hoop(host_guard)
        .push(api_router());
    let app = if browser_enabled {
        app.get(index)
            .push(Router::with_path("files").get(index))
            .push(Router::with_path("offline.html").get(offline_shell))
            .push(Router::with_path("manifest.webmanifest").get(web_manifest))
            .push(Router::with_path("service-worker.js").get(service_worker))
            .push(Router::with_path("assets/offline-boot.js").get(offline_boot))
            .push(Router::with_path("assets/app.css").get(css))
            .push(Router::with_path("assets/app.js").get(javascript))
            .push(Router::with_path("assets/icon.svg").get(icon))
            .push(Router::with_path("assets/icon-192.png").get(icon_192))
            .push(Router::with_path("assets/icon-512.png").get(icon_512))
            .push(Router::with_path("assets/icon-maskable-512.png").get(icon_maskable_512))
            .push(live::router())
    } else {
        app
    };

    println!("Ternilo local web: http://{address}");
    let acceptor = TcpAcceptor::try_from(listener)
        .map_err(|error| HarnessError::execution(format!("create web acceptor: {error}")))?;
    let server = Server::new(acceptor);
    let handle = server.handle();
    let serving = server.try_serve(app);
    tokio::pin!(serving);
    let result = if let Some(shutdown) = http_shutdown {
        tokio::select! {
            result = &mut serving => result,
            () = shutdown.cancelled() => {
                handle.stop_graceful(Some(std::time::Duration::from_secs(2)));
                serving.await
            }
        }
    } else {
        serving.await
    };
    result.map_err(|error| HarnessError::execution(format!("web server: {error}")))
}

fn api_router() -> Router {
    Router::with_path("api/v1")
        .hoop(max_size(MAX_API_BODY_BYTES))
        .hoop(api_auth)
        .push(Router::with_path("health").get(health))
        .push(
            Router::with_path("service")
                .get(service_info)
                .push(Router::with_path("stop").post(stop_service)),
        )
        .push(Router::with_path("catalog").get(catalog))
        .push(Router::with_path("configuration/open").post(open_configuration_directory))
        .push(
            Router::with_path("agent-presets")
                .get(list_agent_presets)
                .post(copy_agent_preset)
                .push(
                    Router::with_path("{preset_id}")
                        .get(get_agent_preset)
                        .put(update_agent_preset)
                        .delete(delete_agent_preset)
                        .push(Router::with_path("default").put(set_default_agent_preset)),
                ),
        )
        .push(Router::with_path("attachments/resolve").post(resolve_attachment))
        .push(extension_router())
        .push(model_connections::router())
        .push(
            Router::with_path("credentials")
                .get(list_credentials)
                .post(set_credential)
                .push(Router::with_path("{name}").delete(remove_credential)),
        )
        .push(
            Router::with_path("credential-records")
                .post(set_credential_record)
                .push(Router::with_path("{scope}/{record_id}").delete(delete_credential_record)),
        )
        .push(
            Router::with_path("providers")
                .get(list_providers)
                .post(upsert_provider)
                .push(Router::with_path("from-extension").post(materialize_extension_provider))
                .push(Router::with_path("discover").post(discover_provider_models))
                .push(Router::with_path("{provider_id}").delete(delete_provider)),
        )
        .push(
            Router::with_path("default-model")
                .get(get_default_model)
                .put(set_default_model),
        )
        .push(
            Router::with_path("sidebar-ordering")
                .get(get_sidebar_ordering)
                .put(set_sidebar_ordering),
        )
        .push(
            Router::with_path("authorizations")
                .get(authorization_snapshot)
                .push(Router::with_path("begin").post(begin_authorization))
                .push(Router::with_path("cancel").post(cancel_authorization)),
        )
        .push(Router::with_path("authorization-prompts/answer").post(answer_authorization_prompt))
        .push(Router::with_path("state").get(local_state))
        .push(Router::with_path("files").get(file_inventory))
        .push(
            Router::with_path("directories")
                .get(directory_listing)
                .post(make_directory),
        )
        .push(
            Router::with_path("workspaces")
                .get(list_workspaces)
                .post(create_workspace)
                .push(
                    Router::with_path("{workspace_id}")
                        .patch(rename_workspace)
                        .delete(unregister_workspace),
                ),
        )
        .push(session_router())
        .push(Router::with_path("session-search").get(search_sessions))
        .push(
            Router::with_path("questions")
                .get(pending_questions)
                .push(Router::with_path("{question_id}/answer").post(answer_question)),
        )
}

fn accepted_hosts(address: SocketAddr) -> BTreeSet<String> {
    let port = address.port();
    [
        address.to_string(),
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
    .into_iter()
    .collect()
}

#[handler]
async fn host_guard(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
    control: &mut FlowCtrl,
) {
    let state = app_state(depot);
    let trusted = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| state.shared.accepted_hosts.contains(host));
    if trusted {
        control.call_next(request, depot, response).await;
    } else {
        response.status_code(StatusCode::FORBIDDEN);
        control.skip_rest();
    }
}

#[handler]
async fn api_auth(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
    control: &mut FlowCtrl,
) {
    let state = app_state(depot);
    let expected = format!("Bearer {}", state.shared.api_token);
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);
    if authorized {
        control.call_next(request, depot, response).await;
    } else {
        response.status_code(StatusCode::UNAUTHORIZED);
        control.skip_rest();
    }
}

fn app_state(depot: &Depot) -> &AppState {
    depot
        .get_typed::<AppState>()
        .expect("application state middleware must run first")
}

fn invalid_request(error: impl std::fmt::Display) -> ApiError {
    HarnessError::invalid(format!("invalid HTTP request: {error}")).into()
}

fn path_parameter(request: &Request, name: &str) -> Result<String, ApiError> {
    request.try_param(name).map_err(invalid_request)
}

#[handler]
fn open_configuration_directory(depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let path = app_state(depot).shared.application.data_dir().to_owned();
    let mut command = if cfg!(target_os = "windows") {
        let mut command = tokio::process::Command::new("cmd");
        command.args(["/C", "start", ""]).arg(path);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = tokio::process::Command::new("open");
        command.arg(path);
        command
    } else {
        let mut command = tokio::process::Command::new("xdg-open");
        command.arg(path);
        command
    };
    let mut child = command.spawn().map_err(|error| {
        HarnessError::execution(format!("open local configuration directory: {error}"))
    })?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

#[handler]
fn service_info(depot: &mut Depot) -> Result<Json<crate::service::ServiceInfo>, ApiError> {
    let control = app_state(depot)
        .shared
        .service
        .as_ref()
        .ok_or_else(|| HarnessError::invalid("this host is not a managed local service"))?;
    Ok(Json(control.info.clone()))
}

#[handler]
fn stop_service(depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let control = app_state(depot)
        .shared
        .service
        .as_ref()
        .ok_or_else(|| HarnessError::invalid("this host is not a managed local service"))?;
    let shutdown = Arc::clone(&control.shutdown);
    // Let the accepted response reach the client before closing the listener.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        shutdown.notify_one();
    });
    Ok(StatusCode::ACCEPTED)
}

#[handler]
fn catalog(depot: &mut Depot) -> Json<ApplicationCatalog> {
    let state = app_state(depot);
    Json(state.shared.application.application_catalog())
}

fn join_error(error: &tokio::task::JoinError) -> ApiError {
    HarnessError::execution(format!("join blocking extension operation: {error}")).into()
}

#[handler]
async fn get_sidebar_ordering(depot: &mut Depot) -> Result<Json<SidebarOrdering>, ApiError> {
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .sidebar_ordering()
            .await?,
    ))
}

#[handler]
async fn set_sidebar_ordering(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SidebarOrdering>, ApiError> {
    let ordering = request
        .parse_json::<SidebarOrdering>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .set_sidebar_ordering(ordering)
            .await?,
    ))
}

#[handler]
async fn local_state(depot: &mut Depot) -> Json<LocalStateSnapshot> {
    let state = app_state(depot);
    Json(state.shared.application.snapshot().await)
}

#[cfg(test)]
mod tests;
