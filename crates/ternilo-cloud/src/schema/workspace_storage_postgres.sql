ALTER TABLE cloud_tenant_storage ENABLE ROW LEVEL SECURITY;
CREATE POLICY cloud_tenant_storage_scope ON cloud_tenant_storage
    USING (tenant_id = current_setting('ternilo.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
        GRANT SELECT, INSERT ON cloud_tenant_storage TO ternilo_runtime;
    END IF;
END $$;
