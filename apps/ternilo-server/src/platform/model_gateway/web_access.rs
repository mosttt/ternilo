use crate::platform::{
    http::{ApiError, bearer_token, invalid_request, now_ms},
    state::app_state,
};
use salvo_core::prelude::{Depot, Json, Request, handler};
use ternilo_protocol::{
    HarnessError, NodeWebAccessRequest, ProviderWebAccessReply, RunModelBinding,
};
use ternilo_transport::{ApplicationOperation, ExecutorId};

#[handler]
pub(super) async fn node_web_access(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ProviderWebAccessReply>, ApiError> {
    let state = app_state(depot);
    let token = bearer_token(request)?;
    let node = state.store.authenticate_node(token, now_ms()?).await?;
    let body = request
        .parse_json::<NodeWebAccessRequest>()
        .await
        .map_err(invalid_request)?;
    if !state
        .store
        .node_model_session_registered(&node, &body.session_id)
        .await?
    {
        crate::platform::workbench::synchronize_node_model_session(state, &node, &body.session_id)
            .await?;
    }
    // Newly created child sessions and schedule events can arrive after the call.
    if let Some(invocation) = &body.invocation {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !state.store.node_run_registered(&node, invocation).await? {
            if tokio::time::Instant::now() >= deadline {
                return Err(HarnessError::unavailable(
                    "web access session has not synchronized; retry after the computer reconnects",
                )
                .into());
            }
            crate::platform::workbench::synchronize_node_model_session(
                state,
                &node,
                &body.session_id,
            )
            .await?;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    if let RunModelBinding::ComputerProvider {
        tenant_id,
        owner_user_id,
        executor_id,
        provider_id,
        ..
    } = &body.binding
    {
        state.store.node_web_snapshot(&node, &body).await?;
        let provider = crate::platform::computer_models::profile(state, &body.binding).await?;
        let api_key = if let Some(capability) = body.credential {
            if owner_user_id != &node.scope.user_id {
                return Err(HarnessError::policy(
                    "web credentials can only be sent to their owner's computers",
                )
                .into());
            }
            let expected = body.expected.ok_or_else(|| {
                HarnessError::policy("web credential requires its selected configuration")
            })?;
            if provider.web_access.as_ref() != Some(&expected) {
                return Err(HarnessError::conflict(
                    "Provider web settings changed; start a new task",
                )
                .into());
            }
            let value = state
                .edge
                .call(
                    tenant_id,
                    &ExecutorId::new(executor_id),
                    ApplicationOperation::ProviderWebCredential {
                        provider_id: provider_id.clone(),
                        capability,
                        expected: *expected,
                    },
                )
                .await?;
            serde_json::from_value(value)
                .map_err(|_| HarnessError::execution("invalid source web credential response"))?
        } else {
            None
        };
        return Ok(Json(ProviderWebAccessReply {
            configuration: provider.web_access,
            api_key,
        }));
    }
    Ok(Json(
        state
            .store
            .node_account_web_access(&node, &body, now_ms()?)
            .await?,
    ))
}
