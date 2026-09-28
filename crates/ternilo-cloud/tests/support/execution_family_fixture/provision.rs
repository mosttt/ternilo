use std::{collections::BTreeSet, time::Duration};

use ternilo_cloud::{CloudStore, WorkerCapacity, WorkerRegisterRequest};
use ternilo_control::{
    ControlStore, InstanceMode, NativeRegistration, OidcPrincipal, TenantQuota, TenantRole,
};
use ternilo_transport::{ExecutorCapability, ExecutorHello, ExecutorId, ExecutorKind};

use super::{Fixture, NOW, WORKER};

impl Fixture {
    pub async fn open(control: ControlStore, cloud: CloudStore) -> Self {
        let owner = control
            .initialize_owner(
                &NativeRegistration {
                    username: "family-owner".to_owned(),
                    email: "family-owner@example.test".to_owned(),
                    password: "family-owner-password".to_owned(),
                },
                NOW,
            )
            .await
            .unwrap()
            .session
            .user;
        control
            .set_instance_mode(&owner, InstanceMode::MultiUser, 1, NOW)
            .await
            .unwrap();
        let actor = control
            .upsert_user(
                &OidcPrincipal {
                    issuer: "https://families.example.test".to_owned(),
                    subject: "actor".to_owned(),
                    email: None,
                    display_name: None,
                },
                "family-actor",
                NOW,
            )
            .await
            .unwrap();
        let tenant = control
            .create_tenant(
                &owner,
                "families",
                "Families",
                TenantQuota {
                    max_nodes: 2,
                    max_concurrent_runs: 4,
                    monthly_model_tokens: 100_000,
                    max_secrets: 2,
                },
                NOW,
            )
            .await
            .unwrap()
            .tenant_id;
        control
            .set_membership(&owner, &tenant, &actor.user_id, TenantRole::Member, NOW)
            .await
            .unwrap();
        let project = control
            .create_project(&owner, &tenant, "Families", NOW)
            .await
            .unwrap()
            .project_id;
        let workspace = control
            .create_cloud_workspace(&owner, &tenant, &project, "Families", NOW)
            .await
            .unwrap()
            .workspace_id;
        let token = cloud
            .create_worker_credential(&ExecutorId::new(WORKER), "family-storage", NOW)
            .await
            .unwrap()
            .token;
        let worker = cloud
            .register_authenticated_worker(
                &token,
                &WorkerRegisterRequest {
                    storage_id: "family-storage".to_owned(),
                    root_id: "family-root".to_owned(),
                    capacity: WorkerCapacity::default(),
                    hello: ExecutorHello {
                        protocol_version: ternilo_transport::EXECUTOR_PROTOCOL_VERSION,
                        executor_id: ExecutorId::new(WORKER),
                        executor_kind: ExecutorKind::CloudWorker,
                        instance_nonce: "family-contract".to_owned(),
                        catalog_revision: "family-contract".to_owned(),
                        capabilities: BTreeSet::from([ExecutorCapability::CloudRun]),
                    },
                },
                Duration::from_secs(300),
                NOW,
            )
            .await
            .unwrap();
        Self {
            control,
            cloud,
            owner,
            actor,
            tenant,
            project,
            workspace,
            worker,
            now: NOW,
        }
    }
}
