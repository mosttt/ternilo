#![allow(
    dead_code,
    reason = "Integration crates compile different portions of this shared model fixture."
)]

use sqlx::Row;
use ternilo_cloud::{CloudStore, StartedRun};
use ternilo_control::{
    ControlStore, ControlUser, ModelRequestInput, ModelRequestPermit, ModelRequestSettlement,
    ModelRequestState, ServiceModelUsage, WorkloadModelPrincipal,
};
use ternilo_kernel::Catalog;
use ternilo_protocol::{
    HarnessError, ModelUsage, PluginEntry, Profile, ProviderModel, ProviderModelDefaults,
    ProviderModelReasoning, ProviderModelSettings, ProviderProfile, ProviderProtocol,
    ReasoningEffort, RunModelBinding, RunModelSnapshot, TenantId, UserId,
};

pub fn snapshot(
    user: &UserId,
    tenant: &TenantId,
    model: &str,
    effort: Option<ReasoningEffort>,
) -> RunModelSnapshot {
    RunModelSnapshot {
        binding: RunModelBinding::UserProvider {
            tenant_id: tenant.clone(),
            owner_user_id: user.clone(),
            provider_id: "contract-provider".to_owned(),
            model: model.to_owned(),
        },
        protocol: ProviderProtocol::OpenAiResponses,
        defaults: ProviderModelDefaults {
            context_window: 4096,
            max_output_tokens: 1024,
            reasoning: Some(ProviderModelReasoning {
                default_effort: ReasoningEffort::Medium,
                efforts: [
                    (ReasoningEffort::Medium, Some("medium".to_owned())),
                    (ReasoningEffort::High, Some("high".to_owned())),
                ]
                .into(),
            }),
        },
        reasoning_effort: effort,
        display_name: model.to_owned(),
        source_name: "Contract Provider".to_owned(),
    }
}

pub async fn configure(
    control: &ControlStore,
    user: &ControlUser,
    tenant: &TenantId,
    model: &str,
    effort: Option<ReasoningEffort>,
    now: u64,
) -> RunModelSnapshot {
    let snapshot = snapshot(&user.user_id, tenant, model, effort);
    control
        .upsert_user_provider_profile(
            user,
            tenant,
            ProviderProfile {
                hosted_tools: None,
                id: "contract-provider".to_owned(),
                display_name: snapshot.source_name.clone(),
                base_url: "https://unused-upstream.example/v1".to_owned(),
                protocol: snapshot.protocol,
                api_key_ref: None,
                defaults: snapshot.defaults.clone(),
                models: vec![ProviderModel {
                    id: model.to_owned(),
                    display_name: None,
                    settings: ProviderModelSettings::Inherit,
                }],
                timeout_ms: 120_000,
                max_attempts: 3,
                retry_base_delay_ms: 1,
            },
            now,
        )
        .await
        .unwrap();
    snapshot
}

pub fn profile(snapshot: &RunModelSnapshot) -> Profile {
    Profile {
        plugins: vec![PluginEntry {
            id: "model".to_owned(),
            kind: ternilo_cloud::BROKERED_MODEL_KIND.to_owned(),
            enabled: true,
            config: serde_json::json!({"snapshot": snapshot}),
        }],
    }
}

pub fn catalog(revision: &str) -> Catalog {
    let mut catalog = Catalog::new(revision);
    catalog
        .register(ternilo_cloud::model_gateway_factory())
        .unwrap();
    catalog
}

/// These store tests act as the trusted Server; HTTP credential and fence admission is covered separately.
pub async fn accept(
    control: &ControlStore,
    cloud: &CloudStore,
    run: &StartedRun,
    worker_id: &str,
    key: &str,
    tokens: u64,
    now: u64,
) -> Result<ModelRequestPermit, HarnessError> {
    let metadata = &run.claim.spec.metadata;
    let snapshot = ternilo_cloud::profile_model_snapshot(&run.claim.spec.profile)?
        .ok_or_else(|| HarnessError::invalid("fixture has no model"))?;
    let mut tx = cloud
        .database()
        .tenant_transaction(&metadata.tenant_id)
        .await?;
    let row = sqlx::query("SELECT r.quota_reservation_id,q.reserved_model_tokens FROM cloud_runs r JOIN control_quota_reservations q ON q.tenant_id=r.tenant_id AND q.reservation_id=r.quota_reservation_id JOIN cloud_session_writer_leases w ON w.tenant_id=r.tenant_id AND w.session_id=r.session_id AND w.run_id=r.run_id WHERE r.tenant_id=$1 AND r.run_id=$2 AND r.state='running' AND r.lease_owner=$3 AND r.lease_token=$4 AND r.session_fencing_token=$5 AND r.lease_expires_at_ms>$6 AND w.fencing_token=$5 AND w.lease_owner=$3 AND w.expires_at_ms>$6")
        .bind(metadata.tenant_id.as_str()).bind(metadata.run_id.as_str()).bind(worker_id).bind(i64::try_from(run.claim.lease_token).unwrap()).bind(i64::try_from(run.fencing_token).unwrap()).bind(i64::try_from(now).unwrap()).fetch_one(&mut *tx).await.map_err(ternilo_storage::database_error)?;
    let principal = WorkloadModelPrincipal {
        tenant_id: metadata.tenant_id.clone(),
        project_id: metadata.project_id.clone().unwrap(),
        workspace_id: metadata.workspace_id.clone(),
        session_id: metadata.session_id.clone(),
        authorization_session_id: run.claim.authorization_session_id.clone(),
        run_id: metadata.run_id.clone(),
        actor_user_id: run.claim.actor_user_id.clone(),
        resource_owner_user_id: metadata.user_id.clone(),
        execution_owner_user_id: metadata.user_id.clone(),
        execution_reservation_id: row
            .try_get("quota_reservation_id")
            .map_err(ternilo_storage::database_error)?,
        worker_id: worker_id.to_owned(),
        worker_generation: 1,
        lease_token: run.claim.lease_token,
        writer_fencing_token: run.fencing_token,
        model: snapshot.binding.clone(),
        run_token_limit: u64::try_from(row.try_get::<i64, _>("reserved_model_tokens").unwrap())
            .unwrap(),
    };
    let permit = control
        .reserve_workload_model_request_in(
            &mut tx,
            &principal,
            &ModelRequestInput {
                request_key: key.to_owned(),
                payload_hash: "a".repeat(64),
                model_id: snapshot.binding.model_id().to_owned(),
                protocol: snapshot.protocol,
                reserved_tokens: tokens,
            },
            now,
        )
        .await
        .map_err(|error| error.error)?;
    if permit.newly_accepted {
        control
            .begin_workload_model_attempt_in(
                &mut tx,
                &principal,
                &permit.request.request_id,
                1,
                now,
            )
            .await
            .map_err(|error| error.error)?;
    }
    tx.commit().await.map_err(ternilo_storage::database_error)?;
    Ok(permit)
}

pub fn settlement(usage: ModelUsage, provider_request: Option<&str>) -> ModelRequestSettlement {
    ModelRequestSettlement {
        state: ModelRequestState::Completed,
        usage: Some(ServiceModelUsage {
            input_tokens: Some(usage.input_tokens),
            output_tokens: Some(usage.output_tokens),
            cached_input_tokens: Some(usage.cached_input_tokens),
            cache_write_tokens: usage.cache_write_tokens,
            reasoning_tokens: Some(usage.reasoning_tokens),
            raw_usage: None,
        }),
        upstream_request_id: provider_request.map(str::to_owned),
        error_code: None,
    }
}

pub async fn settle(
    control: &ControlStore,
    request_id: &str,
    usage: ModelUsage,
    provider_request: Option<&str>,
    now: u64,
) -> Result<(), HarnessError> {
    control
        .settle_model_attempt(request_id, 1, &settlement(usage, provider_request), now)
        .await?;
    control
        .finish_workload_model_request(request_id, ModelRequestState::Completed, None, now)
        .await?;
    Ok(())
}
