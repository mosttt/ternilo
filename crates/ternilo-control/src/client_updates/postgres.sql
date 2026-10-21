ALTER TABLE control_client_updates ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_client_updates_scope ON control_client_updates
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON control_client_updates FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_client_updates TO ternilo_runtime;
END IF;
END $$;
