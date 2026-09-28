CREATE TABLE cloud_worker_credentials (
    worker_id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    storage_id TEXT NOT NULL,
    root_id TEXT,
    created_at_ms BIGINT NOT NULL,
    revoked_at_ms BIGINT
);
CREATE INDEX cloud_worker_credentials_storage ON cloud_worker_credentials(storage_id);

CREATE TABLE cloud_storage_roots (
    storage_id TEXT PRIMARY KEY,
    root_id TEXT NOT NULL,
    registered_at_ms BIGINT NOT NULL
);
