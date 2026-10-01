use salvo_core::prelude::{Depot, Json, Request, Router, StatusCode, handler};
use serde::{Deserialize, Serialize};
use ternilo_control::{
    CandidatePage, PageQuery, ResourceAccess, ResourceAction, ResourceKind, ResourceOwnership,
    ResourceOwnershipTransfer, ResourcePermissions, SharedGrant,
};
use ternilo_protocol::{HarnessError, UserId, WorkspaceId};

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter, tenant_parameter},
    state::{actor, app_state},
};

pub(crate) fn router() -> Router {
    Router::with_path("sharing")
        .get(get_sharing)
        .push(Router::with_path("candidates").get(get_candidates))
        .push(
            Router::with_path("ownership")
                .get(get_ownership)
                .put(transfer_ownership)
                .push(Router::with_path("candidates").get(get_transfer_candidates)),
        )
        .push(
            Router::with_path("{subject_kind}/{subject_id}")
                .put(set_sharing)
                .delete(remove_sharing),
        )
}

#[handler]
async fn get_ownership(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ResourceOwnership>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let (kind, id) = resource(request)?;
    Ok(Json(
        app_state(depot)
            .store
            .resource_ownership(actor(depot), &tenant, kind, &id)
            .await?,
    ))
}

#[handler]
async fn transfer_ownership(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ResourceOwnership>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let (kind, id) = resource(request)?;
    let body = request
        .parse_json::<ResourceOwnershipTransfer>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let ownership = state
        .store
        .transfer_resource_ownership(actor(depot), &tenant, kind, &id, &body, now_ms()?)
        .await?;
    state.edge.notify_resource_change(&tenant);
    Ok(Json(ownership))
}

#[handler]
async fn get_transfer_candidates(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<CandidatePage>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let (kind, id) = resource(request)?;
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .resource_transfer_candidates(actor(depot), &tenant, kind, &id, &query)
            .await?,
    ))
}

fn resource(request: &Request) -> Result<(ResourceKind, String), ApiError> {
    if let Some(id) = request.param::<String>("session_id") {
        Ok((ResourceKind::Session, id))
    } else if let Some(id) = request.param::<String>("project_id") {
        Ok((ResourceKind::Project, id))
    } else {
        Ok((
            ResourceKind::Workspace,
            path_parameter(request, "workspace_id")?,
        ))
    }
}

#[derive(Serialize)]
struct SharingSnapshot {
    access: ResourceAccess,
    shares: Vec<SharedGrant>,
    next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_inheritance: Option<ternilo_control::ProjectSharingInheritance>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateQuery {
    kind: String,
    query: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

#[handler]
async fn get_sharing(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SharingSnapshot>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let (kind, id) = resource(request)?;
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    let store = &app_state(depot).store;
    let user = actor(depot);
    let access = store.resource_access(user, &tenant_id, kind, &id).await?;
    access.require(ResourceAction::View)?;
    let (shares, next_cursor) = if access.can_manage_sharing || kind == ResourceKind::Project {
        let page = store
            .list_resource_shares(user, &tenant_id, kind, &id, &query)
            .await?;
        (page.shares, page.next_cursor)
    } else {
        (Vec::new(), None)
    };
    let project_inheritance = if kind == ResourceKind::Workspace {
        Some(
            store
                .workspace_project_sharing(user, &tenant_id, &WorkspaceId::new(&id))
                .await?,
        )
    } else {
        None
    };
    Ok(Json(SharingSnapshot {
        access,
        shares,
        next_cursor,
        project_inheritance,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetProjectInheritance {
    enabled: bool,
}

#[handler]
pub(super) async fn set_project_inheritance(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_control::ProjectSharingInheritance>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let workspace = WorkspaceId::new(path_parameter(request, "workspace_id")?);
    let body = request
        .parse_json::<SetProjectInheritance>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let inheritance = state
        .store
        .set_workspace_project_sharing(actor(depot), &tenant, &workspace, body.enabled, now_ms()?)
        .await?;
    state.edge.notify_resource_change(&tenant);
    Ok(Json(inheritance))
}

#[handler]
async fn get_candidates(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<CandidatePage>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let (kind, id) = resource(request)?;
    let query = request
        .parse_queries::<CandidateQuery>()
        .map_err(invalid_request)?;
    let page = PageQuery {
        query: query.query,
        cursor: query.cursor,
        limit: query.limit.unwrap_or_else(|| PageQuery::default().limit),
    };
    Ok(Json(
        app_state(depot)
            .store
            .resource_share_candidates(actor(depot), &tenant_id, kind, &id, &query.kind, &page)
            .await?,
    ))
}

#[handler]
async fn set_sharing(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let permissions = request
        .parse_json::<ResourcePermissions>()
        .await
        .map_err(invalid_request)?;
    update_sharing(request, depot, Some(permissions)).await
}

#[handler]
async fn remove_sharing(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    update_sharing(request, depot, None).await
}

async fn update_sharing(
    request: &Request,
    depot: &Depot,
    permissions: Option<ResourcePermissions>,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let (kind, id) = resource(request)?;
    let subject_kind = path_parameter(request, "subject_kind")?;
    let subject_id = path_parameter(request, "subject_id")?;
    let state = app_state(depot);
    match subject_kind.as_str() {
        "user" => {
            state
                .store
                .set_resource_share(
                    actor(depot),
                    &tenant_id,
                    kind,
                    &id,
                    &UserId::new(subject_id),
                    permissions,
                    now_ms()?,
                )
                .await?;
        }
        "group" => {
            state
                .store
                .set_resource_group_share(
                    actor(depot),
                    &tenant_id,
                    kind,
                    &id,
                    &subject_id,
                    permissions,
                    now_ms()?,
                )
                .await?;
        }
        _ => {
            return Err(HarnessError::invalid("sharing subject kind must be user or group").into());
        }
    }
    state.edge.notify_resource_change(&tenant_id);
    Ok(StatusCode::NO_CONTENT)
}
