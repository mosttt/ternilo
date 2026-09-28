CREATE TABLE control_extension_publishers (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    key_id TEXT NOT NULL,
    trust JSONB NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    added_at_ms BIGINT NOT NULL CHECK (added_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= added_at_ms),
    added_by TEXT NOT NULL REFERENCES control_users(user_id),
    PRIMARY KEY (tenant_id, key_id)
);

CREATE TABLE control_extension_packages (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    package_id TEXT NOT NULL,
    version TEXT NOT NULL,
    publisher_key_id TEXT NOT NULL,
    install_request JSONB NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    installed_at_ms BIGINT NOT NULL CHECK (installed_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= installed_at_ms),
    installed_by TEXT NOT NULL REFERENCES control_users(user_id),
    PRIMARY KEY (tenant_id, package_id, version),
    FOREIGN KEY (tenant_id, publisher_key_id)
        REFERENCES control_extension_publishers(tenant_id, key_id)
);

CREATE INDEX control_extension_packages_publisher
ON control_extension_packages (tenant_id, publisher_key_id);

ALTER TABLE control_extension_publishers ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_extension_packages ENABLE ROW LEVEL SECURITY;

CREATE POLICY control_extension_publishers_scope ON control_extension_publishers
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_extension_packages_scope ON control_extension_packages
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
