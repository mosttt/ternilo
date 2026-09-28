use super::route;
use crate::platform::{http::now_ms, state::AppState};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use ternilo_control::{ModelRequestInput, NodeModelPrincipal, NodePrincipal};
use ternilo_protocol::{HarnessError, NodeModelRequest};
use ternilo_storage::{Transaction, database_error};

pub(super) struct NodeAuthority {
    node: NodePrincipal,
    body: NodeModelRequest,
    principal: NodeModelPrincipal,
}

impl NodeAuthority {
    pub(super) async fn accept(
        state: &AppState,
        token: &str,
        body: &NodeModelRequest,
    ) -> Result<(Self, String, route::ResolvedBrokeredRoute), HarnessError> {
        if body.request_id.is_empty() || body.request_id.len() > 128 {
            return Err(HarnessError::invalid(
                "Node model request ID must contain 1 to 128 characters",
            ));
        }
        let now = now_ms()?;
        let node = state.store.authenticate_node(token, now).await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !state
            .store
            .node_model_request_registered(&node, body)
            .await?
        {
            if tokio::time::Instant::now() >= deadline {
                return Err(HarnessError::unavailable(
                    "Node model session or schedule origin has not synchronized; retry after the computer reconnects",
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let mut tx = state.store.database().begin().await?;
        let principal = state
            .store
            .authorize_node_model_in(&mut tx, &node, body)
            .await?;
        let mut canonical = serde_json::to_value(&body.request)
            .map_err(|error| HarnessError::invalid(error.to_string()))?;
        canonical.sort_all_objects();
        let payload_hash = Sha256::digest(canonical.to_string().as_bytes())
            .iter()
            .fold(String::new(), |mut text, byte| {
                write!(text, "{byte:02x}").expect("String write");
                text
            });
        let input = ModelRequestInput {
            request_key: body.request_id.clone(),
            payload_hash,
            model_id: body.binding.model_id().to_owned(),
            protocol: principal.snapshot.protocol,
            reserved_tokens: route::conservative_model_budget(&body.request, &principal.snapshot)?,
        };
        let permit = state
            .store
            .reserve_node_model_request_in(&mut tx, &principal, &input, now)
            .await
            .map_err(|error| error.error)?;
        if !permit.newly_accepted {
            return Err(HarnessError::conflict(
                "Node model request was already accepted; upstream work cannot be replayed",
            ));
        }
        let resolved = route::resolve(permit.route, &principal.snapshot)?;
        tx.commit().await.map_err(database_error)?;
        // Authorization checks need identity and provenance, never the prompt itself.
        let mut identity = body.clone();
        identity.request.messages.clear();
        identity.request.tools.clear();
        identity.request.system_prompt.clear();
        Ok((
            Self {
                node,
                body: identity,
                principal,
            },
            permit.request.request_id,
            resolved,
        ))
    }

    async fn authorize(&self, state: &AppState, tx: &mut Transaction) -> Result<(), HarnessError> {
        let current = state
            .store
            .authorize_node_model_in(tx, &self.node, &self.body)
            .await?;
        if current != self.principal {
            return Err(HarnessError::policy(
                "the accepted Node model authority changed",
            ));
        }
        Ok(())
    }

    pub(super) async fn begin_attempt(
        &self,
        state: &AppState,
        id: &str,
        attempt: u32,
    ) -> Result<(), HarnessError> {
        let mut tx = state.store.database().begin().await?;
        self.authorize(state, &mut tx).await?;
        state
            .store
            .begin_node_model_attempt_in(&mut tx, &self.principal, id, attempt, now_ms()?)
            .await
            .map_err(|error| error.error)?;
        tx.commit().await.map_err(database_error)
    }

    pub(super) async fn check(&self, state: &AppState, id: &str) -> Result<(), HarnessError> {
        let mut tx = state.store.database().begin().await?;
        self.authorize(state, &mut tx).await?;
        state
            .store
            .check_node_model_request_in(&mut tx, &self.principal, id, now_ms()?)
            .await
            .map_err(|error| error.error)?;
        tx.commit().await.map_err(database_error)
    }
}
