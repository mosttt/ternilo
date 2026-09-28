-- Keep the high-water mark independently of run and occupancy history.
CREATE TABLE cloud_workspace_epochs (
    storage_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    occupation_epoch BIGINT NOT NULL CHECK (occupation_epoch > 0),
    PRIMARY KEY(storage_id,tenant_id,workspace_id)
);

-- Persistent physical workspace ownership. A missing confirmation never frees this row.
CREATE TABLE cloud_workspace_occupancy (
    storage_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    family_id TEXT NOT NULL,
    worker_id TEXT NOT NULL,
    worker_generation BIGINT NOT NULL CHECK (worker_generation > 0),
    occupation_epoch BIGINT NOT NULL CHECK (occupation_epoch > 0),
    run_id TEXT NOT NULL,
    lease_token BIGINT NOT NULL CHECK (lease_token > 0),
    state TEXT NOT NULL CHECK (state IN ('held','cleanup','released')),
    exit_confirmed_at_ms BIGINT,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY(tenant_id,run_id,lease_token)
);
CREATE INDEX cloud_workspace_occupancy_live ON cloud_workspace_occupancy(storage_id,tenant_id,workspace_id,state);
CREATE INDEX cloud_workspace_occupancy_family ON cloud_workspace_occupancy(tenant_id,family_id,state);
