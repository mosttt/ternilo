use std::{fmt::Write as _, sync::Arc};

use sha2::{Digest as _, Sha256};
use ternilo_cloud::{CloudWorkerIdentity, RunLease, WorkerModelRequest};
use ternilo_control::{ModelRequestInput, WorkloadModelPrincipal};
use ternilo_protocol::{HarnessError, RunModelBinding};
use ternilo_storage::{Transaction, database_error};
use zeroize::Zeroizing;

use crate::platform::{http::now_ms, state::AppState};

use super::route;

pub(super) struct AcceptedModelCall {
    pub(super) state: AppState,
    pub(super) request_id: String,
    pub(super) binding: RunModelBinding,
    authority: Authority,
}

enum Authority {
    Worker(Box<WorkerAuthority>),
    Node(Box<super::node_access::NodeAuthority>),
}

struct WorkerAuthority {
    principal: WorkloadModelPrincipal,
    worker_token: Zeroizing<String>,
    identity: CloudWorkerIdentity,
    lease: RunLease,
}

impl AcceptedModelCall {
    pub(super) async fn accept(
        state: AppState,
        worker_token: String,
        body: &WorkerModelRequest,
    ) -> Result<(Arc<Self>, route::ResolvedBrokeredRoute), HarnessError> {
        if body.request_id == 0 {
            return Err(HarnessError::invalid(
                "Worker model request ID must be positive",
            ));
        }
        body.binding.validate()?;
        let now = now_ms()?;
        let mut transaction = state.store.database().begin().await?;
        let (run, principal) = state
            .cloud
            .authorize_workload_model_in(
                &mut transaction,
                &worker_token,
                &body.identity,
                &body.run,
                &body.binding,
                now,
            )
            .await?;
        let snapshot = route::snapshot(&run, &body.binding)?;
        let mut canonical = serde_json::to_value(&body.request)
            .map_err(|error| HarnessError::invalid(format!("encode model request: {error}")))?;
        canonical.sort_all_objects();
        let payload_hash = Sha256::digest(canonical.to_string().as_bytes())
            .iter()
            .fold(String::with_capacity(64), |mut text, byte| {
                write!(text, "{byte:02x}").expect("writing to a String cannot fail");
                text
            });
        let input = ModelRequestInput {
            request_key: body.request_id.to_string(),
            payload_hash,
            model_id: body.binding.model_id().to_owned(),
            protocol: snapshot.protocol,
            reserved_tokens: route::conservative_model_budget(&body.request, &snapshot)?,
        };
        let permit = state
            .store
            .reserve_workload_model_request_in(&mut transaction, &principal, &input, now)
            .await
            .map_err(|error| error.error)?;
        if !permit.newly_accepted {
            return Err(HarnessError::conflict(
                "Worker model request was already accepted; upstream work cannot be replayed",
            ));
        }
        let resolved = route::resolve(permit.route, &snapshot)?;
        transaction.commit().await.map_err(database_error)?;
        Ok((
            Arc::new(Self {
                state,
                request_id: permit.request.request_id,
                binding: body.binding.clone(),
                authority: Authority::Worker(Box::new(WorkerAuthority {
                    principal,
                    worker_token: Zeroizing::new(worker_token),
                    identity: body.identity.clone(),
                    lease: body.run.clone(),
                })),
            }),
            resolved,
        ))
    }

    pub(super) async fn accept_node(
        state: AppState,
        token: String,
        body: &ternilo_protocol::NodeModelRequest,
    ) -> Result<(Arc<Self>, route::ResolvedBrokeredRoute), HarnessError> {
        let (authority, request_id, resolved) =
            super::node_access::NodeAuthority::accept(&state, &token, body).await?;
        Ok((
            Arc::new(Self {
                state,
                request_id,
                binding: body.binding.clone(),
                authority: Authority::Node(Box::new(authority)),
            }),
            resolved,
        ))
    }

    pub(super) async fn finish(
        &self,
        status: ternilo_control::ModelRequestState,
        error: Option<&str>,
        now: u64,
    ) -> Result<(), HarnessError> {
        match &self.authority {
            Authority::Worker(_) => {
                self.state
                    .store
                    .finish_workload_model_request(&self.request_id, status, error, now)
                    .await?
            }
            Authority::Node(_) => {
                self.state
                    .store
                    .finish_node_model_request(&self.request_id, status, error, now)
                    .await?
            }
        };
        Ok(())
    }

    async fn authorize(
        &self,
        transaction: &mut Transaction,
        now: u64,
    ) -> Result<WorkloadModelPrincipal, HarnessError> {
        let Authority::Worker(authority) = &self.authority else {
            return Err(HarnessError::policy("expected Worker authority"));
        };
        let (_, principal) = self
            .state
            .cloud
            .authorize_workload_model_in(
                transaction,
                &authority.worker_token,
                &authority.identity,
                &authority.lease,
                &self.binding,
                now,
            )
            .await?;
        // Steering can increase an accepted run's ceiling without changing the
        // actor, model allowance, execution owner or lease that authorized it.
        let mut accepted = authority.principal.clone();
        accepted.run_token_limit = principal.run_token_limit;
        if principal != accepted {
            return Err(HarnessError::policy(
                "the accepted workload model identity changed",
            ));
        }
        Ok(principal)
    }

    pub(super) async fn begin_attempt(&self, attempt: u32) -> Result<(), HarnessError> {
        if let Authority::Node(authority) = &self.authority {
            return authority
                .begin_attempt(&self.state, &self.request_id, attempt)
                .await;
        }
        let now = now_ms()?;
        let mut transaction = self.state.store.database().begin().await?;
        let principal = self.authorize(&mut transaction, now).await?;
        self.state
            .store
            .begin_workload_model_attempt_in(
                &mut transaction,
                &principal,
                &self.request_id,
                attempt,
                now,
            )
            .await
            .map_err(|error| error.error)?;
        transaction.commit().await.map_err(database_error)
    }

    pub(super) async fn check(&self) -> Result<(), HarnessError> {
        if let Authority::Node(authority) = &self.authority {
            return authority.check(&self.state, &self.request_id).await;
        }
        let now = now_ms()?;
        let mut transaction = self.state.store.database().begin().await?;
        let principal = self.authorize(&mut transaction, now).await?;
        self.state
            .store
            .check_workload_model_request_in(&mut transaction, &principal, &self.request_id, now)
            .await
            .map_err(|error| error.error)?;
        transaction.commit().await.map_err(database_error)
    }
}
