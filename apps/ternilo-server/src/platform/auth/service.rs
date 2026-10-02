use salvo_core::prelude::Request;
use ternilo_control::{ServicePrincipal, ServiceScope};
use ternilo_protocol::HarnessError;

use crate::platform::{
    http::{ApiError, now_ms, tenant_parameter},
    state::AppState,
};

pub(super) async fn authenticate_request(
    request: &Request,
    state: &AppState,
    token: &str,
) -> Result<ServicePrincipal, ApiError> {
    let tenant = tenant_parameter(request)?;
    if request
        .headers()
        .get("x-ternilo-tenant")
        .is_some_and(|header| header.to_str().ok() != Some(tenant.as_str()))
    {
        return Err(HarnessError::policy(
            "service credential tenant header does not match the requested space",
        )
        .into());
    }
    let principal = state
        .store
        .authenticate_service_credential(token, &tenant, now_ms()?)
        .await
        .map_err(super::authentication_error)?;
    let scope = route_scope(request).ok_or_else(|| {
        HarnessError::policy("this endpoint is not available to service credentials")
    })?;
    principal.require(scope)?;
    Ok(principal)
}

fn route_scope(request: &Request) -> Option<ServiceScope> {
    let path = request.uri().path().strip_prefix("/api/v1/")?;
    let parts: Vec<_> = path.split('/').collect();
    let parts = if parts.first() == Some(&"tenants") {
        parts.get(2..)?
    } else {
        parts.as_slice()
    };
    match (request.method().as_str(), parts) {
        (
            "GET",
            ["me" | "projects" | "workspaces" | "sessions" | "runs"]
            | ["workspaces" | "runs", _]
            | ["workspaces", _, "location"]
            | [
                "sessions",
                _,
                "history" | "events" | "stats" | "projection" | "queue",
            ]
            | ["sessions", _, "files", _, "content"],
        ) => Some(ServiceScope::ResourceRead),
        (
            "POST",
            ["workspaces" | "sessions" | "runs"]
            | ["sessions", _, "turns" | "queue"]
            | ["sessions", _, "queue", _, "steer"]
            | ["runs", "chat"],
        )
        | ("PATCH" | "DELETE", ["sessions", _, "queue", _])
        | ("DELETE", ["sessions", _, "turns", _] | ["runs", _]) => Some(ServiceScope::RunExecute),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use salvo_core::test::TestClient;

    #[test]
    fn service_routes_keep_execution_separate_from_read_and_deny_management_and_unscoped_channels()
    {
        for route in [
            "projects",
            "workspaces/w/location",
            "sessions/s/history",
            "sessions/s/queue",
            "tenants/tenant/runs/run",
        ] {
            assert_eq!(
                route_scope(&TestClient::get(format!("http://server.test/api/v1/{route}")).build()),
                Some(ServiceScope::ResourceRead)
            );
        }
        for route in [
            "sessions/s/queue",
            "sessions/s/turns",
            "tenants/tenant/runs/chat",
        ] {
            assert_eq!(
                route_scope(
                    &TestClient::post(format!("http://server.test/api/v1/{route}")).build()
                ),
                Some(ServiceScope::RunExecute)
            );
        }
        for route in [
            "auth/session",
            "auth/sessions",
            "admin/instance",
            "providers",
            "credentials",
            "live",
            "tenants/t/service-accounts",
            "sessions/s/shares",
            "tenants/t/enrollments",
            "sessions/s/workspace",
        ] {
            assert_eq!(
                route_scope(&TestClient::get(format!("http://server.test/api/v1/{route}")).build()),
                None
            );
            assert_eq!(
                route_scope(
                    &TestClient::post(format!("http://server.test/api/v1/{route}")).build()
                ),
                None
            );
        }
    }
}
