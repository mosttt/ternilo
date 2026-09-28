CREATE TABLE cloud_run_execution (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id),
    run_id TEXT NOT NULL,
    lease_token BIGINT NOT NULL CHECK (lease_token > 0),
    session_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    actor_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    storage_id TEXT NOT NULL,
    worker_id TEXT NOT NULL,
    worker_generation BIGINT NOT NULL CHECK (worker_generation > 0),
    writer_fencing_token BIGINT NOT NULL DEFAULT 0 CHECK (writer_fencing_token >= 0),
    phase TEXT NOT NULL CHECK (phase IN ('claimed','active','parked','resume_pending','cleanup','lost','released')),
    admission_epoch BIGINT NOT NULL DEFAULT 1 CHECK (admission_epoch > 0),
    activity_revision BIGINT NOT NULL DEFAULT 0 CHECK (activity_revision >= 0),
    parked_revision BIGINT NOT NULL DEFAULT 0 CHECK (parked_revision >= 0),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    released_at_ms BIGINT,
    PRIMARY KEY (tenant_id, run_id, lease_token),
    FOREIGN KEY (tenant_id, run_id) REFERENCES cloud_run_lineage(tenant_id, run_id)
);
CREATE INDEX cloud_run_execution_worker ON cloud_run_execution(worker_id, phase);
CREATE INDEX cloud_run_execution_tenant ON cloud_run_execution(tenant_id, phase);
CREATE INDEX cloud_run_execution_storage ON cloud_run_execution(storage_id, phase);

CREATE TABLE cloud_run_wait_dependencies (
    tenant_id TEXT NOT NULL,
    parent_run_id TEXT NOT NULL,
    parent_lease_token BIGINT NOT NULL,
    owner_user_id TEXT NOT NULL,
    activity_revision BIGINT NOT NULL,
    child_session_id TEXT NOT NULL,
    child_run_id TEXT NOT NULL,
    PRIMARY KEY (tenant_id, parent_run_id, parent_lease_token, child_run_id),
    FOREIGN KEY (tenant_id, parent_run_id, parent_lease_token)
        REFERENCES cloud_run_execution(tenant_id, run_id, lease_token) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, child_run_id) REFERENCES cloud_run_lineage(tenant_id, run_id)
);

CREATE INDEX cloud_run_wait_dependencies_child ON cloud_run_wait_dependencies(tenant_id,child_run_id);
