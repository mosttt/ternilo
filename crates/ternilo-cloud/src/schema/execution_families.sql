CREATE TABLE cloud_execution_families (
 tenant_id TEXT NOT NULL,
 session_id TEXT NOT NULL,
 owner_user_id TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 family_id TEXT NOT NULL,
 created_at_ms BIGINT NOT NULL,
 PRIMARY KEY (tenant_id,session_id)
);
CREATE INDEX cloud_execution_families_family ON cloud_execution_families(tenant_id,family_id);
