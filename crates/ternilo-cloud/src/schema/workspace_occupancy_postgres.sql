ALTER TABLE cloud_workspace_occupancy ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_workspace_epochs ENABLE ROW LEVEL SECURITY;
CREATE POLICY cloud_workspace_epochs_scope ON cloud_workspace_epochs
 USING (tenant_id = current_setting('ternilo.tenant_id', true))
 WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
CREATE POLICY cloud_workspace_occupancy_scope ON cloud_workspace_occupancy
 USING (tenant_id = current_setting('ternilo.tenant_id', true))
 WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
DO $$ BEGIN IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
 GRANT SELECT,INSERT,UPDATE,DELETE ON cloud_workspace_occupancy TO ternilo_runtime;
 GRANT SELECT,INSERT,UPDATE ON cloud_workspace_epochs TO ternilo_runtime;
END IF; END $$;
