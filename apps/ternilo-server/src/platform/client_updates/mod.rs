use super::{
    actor, app_state,
    http::{ApiError, bearer_token, invalid_request, now_ms, path_parameter, tenant_parameter},
};
use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Response, Router, handler},
};
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_control::ClientUpdateJob;
use ternilo_protocol::{
    ClientUpdateInfo, ClientUpdatePhase, ClientUpdateProgress, HarnessError, TenantId,
};
use ternilo_transport::{
    ClientUpdateCommand, EXECUTOR_PROTOCOL_VERSION, ExecutorId, MaintenanceOperation,
};
use ternilo_update::SignedRelease;
mod cache;
mod maintenance;
pub(crate) use cache::UpdateCache;
pub(super) use maintenance::router as maintenance_router;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    id: String,
}

#[handler]
pub(super) async fn status(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let executor = ExecutorId::new(path_parameter(request, "executor_id")?);
    let state = app_state(depot);
    let details = state
        .store
        .computer_details(actor(depot), &tenant, &executor, true)
        .await?;
    let execution_connected = state.edge.is_connected(&tenant, &executor).await;
    let maintenance = state
        .store
        .maintenance_state(actor(depot), &tenant, &executor)
        .await?;
    let now = now_ms()?;
    let connected = details.executor.state != "revoked"
        && details.management.suspended_at_ms.is_none()
        && details.management.removed_at_ms.is_none()
        && maintenance.as_ref().is_some_and(|state| state.online(now));
    let execution_protocol_version = maintenance
        .as_ref()
        .map(|state| state.hello.execution_protocol_version)
        .or_else(|| details.hello.as_ref().map(|hello| hello.protocol_version));
    let has_maintenance = maintenance.is_some();
    let mut info = maintenance
        .map(|state| state.hello.info)
        .or_else(|| details.hello.and_then(|hello| hello.client));
    if connected && info.is_some() && request.query::<bool>("refresh").unwrap_or(false) {
        info = Some(
            maintenance::call(
                state,
                actor(depot),
                &tenant,
                &executor,
                MaintenanceOperation::Status,
            )
            .await?,
        );
    }
    let mut job = state
        .store
        .client_update_job(actor(depot), &tenant, &executor)
        .await?;
    if let Some(current) = &job
        && current.progress.phase == ClientUpdatePhase::Preparing
        && current.created_at_ms.saturating_add(15 * 60 * 1000) < now_ms()?
    {
        state
            .store
            .fail_client_update_preparation(
                &tenant,
                &executor,
                &current.progress.id,
                "Server preparation was interrupted; retry the update",
            )
            .await?;
        job = state
            .store
            .client_update_job(actor(depot), &tenant, &executor)
            .await?;
    }
    let availability = match &info {
        _ if !connected
            && !has_maintenance
            && execution_protocol_version
                .is_some_and(|version| version != EXECUTOR_PROTOCOL_VERSION) =>
        {
            "manual_upgrade"
        }
        _ if !connected => "offline",
        None => "unsupported",
        Some(info) if !info.enabled => "not_enabled",
        Some(info) if info.target.is_none() => "unsupported",
        Some(info) => match (
            semver::Version::parse(&info.version),
            semver::Version::parse(env!("CARGO_PKG_VERSION")),
        ) {
            (Ok(current), Ok(target)) if current < target => "available",
            (Ok(current), Ok(target)) if current == target => "current",
            (Ok(_), Ok(_)) => "newer",
            _ => "unsupported",
        },
    };
    Ok(Json(
        json!({"connected":connected,"execution_connected":execution_connected,"execution_protocol_version":execution_protocol_version,"server_execution_protocol_version":EXECUTOR_PROTOCOL_VERSION,"info":info,"job":job.map(|job|job.progress),"target_version":env!("CARGO_PKG_VERSION"),"availability":availability}),
    ))
}

#[handler]
pub(super) async fn start(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant = tenant_parameter(request)?;
    let executor = ExecutorId::new(path_parameter(request, "executor_id")?);
    let body: StartRequest = request.parse_json().await.map_err(invalid_request)?;
    ClientUpdateCommand::Cancel {
        id: body.id.clone(),
    }
    .validate()?;
    let state = app_state(depot);
    state
        .store
        .computer_details(actor(depot), &tenant, &executor, true)
        .await?;
    if let Some(previous) = state
        .store
        .client_update_job(actor(depot), &tenant, &executor)
        .await?
        && previous.progress.id == body.id
    {
        return Ok((StatusCode::ACCEPTED, Json(json!({"job":previous.progress}))));
    }
    let info = maintenance::call(
        state,
        actor(depot),
        &tenant,
        &executor,
        MaintenanceOperation::Status,
    )
    .await?;
    if info
        .progress
        .as_ref()
        .is_some_and(|progress| !progress.phase.terminal() && progress.id != body.id)
    {
        return Err(HarnessError::conflict("this computer already has an active update").into());
    }
    if !info.enabled {
        return Err(HarnessError::policy(
            "the computer owner must enable remote updates locally and restart the client once",
        )
        .into());
    }
    let target = info
        .target
        .as_deref()
        .ok_or_else(|| HarnessError::invalid("computer has no supported client update package"))?;
    // Recommend the exact tested Server release rather than following a moving latest tag.
    let release = state
        .client_updates
        .release(env!("CARGO_PKG_VERSION"))
        .await?;
    release.validate_upgrade(&info.version, env!("CARGO_PKG_VERSION"), info.data_epoch)?;
    release.artifact(target)?;
    let now = now_ms()?;
    let token = format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    );
    let job = ClientUpdateJob {
        progress: ClientUpdateProgress {
            id: body.id.clone(),
            version: release.manifest().version.clone(),
            phase: ClientUpdatePhase::Preparing,
            revision: 0,
            error: None,
        },
        target: target.to_owned(),
        manifest: release.signed().manifest.clone(),
        signature: release.signed().signature.clone(),
        created_at_ms: now,
        expires_at_ms: now.saturating_add(24 * 60 * 60 * 1000),
    };
    if state
        .store
        .create_client_update_job(actor(depot), &tenant, &executor, &job, &token)
        .await?
    {
        let actor = actor(depot).clone();
        let state = state.clone();
        let tenant = tenant.clone();
        let executor = executor.clone();
        let job = job.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(std::time::Duration::from_mins(14), async {
                state.client_updates.package(&release, &job.target).await?;
                maintenance::call(
                    &state,
                    &actor,
                    &tenant,
                    &executor,
                    MaintenanceOperation::Start {
                        id: job.progress.id.clone(),
                    },
                )
                .await?;
                Ok::<(), HarnessError>(())
            })
            .await
            .unwrap_or_else(|_| {
                Err(HarnessError::unavailable(
                    "Server update preparation timed out; retry the update",
                ))
            });
            if let Err(error) = result {
                let _ = state
                    .store
                    .fail_client_update_preparation(
                        &tenant,
                        &executor,
                        &job.progress.id,
                        &error.to_string(),
                    )
                    .await;
            }
            state.edge.notify_computer_changed(&tenant, &executor);
        });
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"job":job.progress}))))
}

#[handler]
pub(super) async fn cancel(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let executor = ExecutorId::new(path_parameter(request, "executor_id")?);
    let id = path_parameter(request, "id")?;
    let state = app_state(depot);
    state
        .store
        .computer_details(actor(depot), &tenant, &executor, true)
        .await?;
    Ok(Json(
        serde_json::to_value(
            maintenance::call(
                state,
                actor(depot),
                &tenant,
                &executor,
                MaintenanceOperation::Cancel { id },
            )
            .await?,
        )
        .map_err(invalid_request)?,
    ))
}

pub(super) fn download_router() -> Router {
    Router::with_path("api/v1/executors/client-updates/{tenant_id}/{executor_id}/{id}/package")
        .hoop(super::identity::no_store)
        .get(download)
}
#[handler]
async fn download(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<(), ApiError> {
    let tenant = TenantId::new(path_parameter(request, "tenant_id")?);
    let executor = ExecutorId::new(path_parameter(request, "executor_id")?);
    let id = path_parameter(request, "id")?;
    let token = bearer_token(request)?;
    let state = app_state(depot);
    let job = state
        .store
        .client_update_download(&tenant, &executor, &id, token, now_ms()?)
        .await
        .map_err(ApiError::unauthorized)?;
    let release = SignedRelease {
        manifest: job.manifest,
        signature: job.signature,
    }
    .verify()?;
    let path = state.client_updates.package(&release, &job.target).await?;
    let file = tokio::fs::File::open(path).await.map_err(|_| {
        HarnessError::unavailable("verified update package is not available in cache")
    })?;
    response
        .add_header("content-type", "application/octet-stream", true)
        .map_err(|_| HarnessError::execution("set update download content type"))?;
    response
        .add_header(
            "content-length",
            release.artifact(&job.target)?.bytes.to_string(),
            true,
        )
        .map_err(|_| HarnessError::execution("set update download length"))?;
    response.stream(tokio_util::io::ReaderStream::new(file));
    Ok(())
}
