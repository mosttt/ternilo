ALTER TABLE control_client_maintenance ENABLE ROW LEVEL SECURITY;
CREATE POLICY client_maintenance_scope ON control_client_maintenance
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
ALTER TABLE control_maintenance_commands ENABLE ROW LEVEL SECURITY;
CREATE POLICY maintenance_commands_scope ON control_maintenance_commands
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON control_client_maintenance, control_maintenance_commands FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_client_maintenance, control_maintenance_commands TO ternilo_runtime;
END IF;
END $$;
