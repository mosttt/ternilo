CREATE TABLE cloud_tenant_storage (
    tenant_id TEXT PRIMARY KEY,
    storage_id TEXT NOT NULL,
    assigned_at_ms BIGINT NOT NULL
);

CREATE INDEX cloud_tenant_storage_location ON cloud_tenant_storage(storage_id);
