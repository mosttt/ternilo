ALTER TABLE control_computer_management ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_computer_management_scope ON control_computer_management
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON control_computer_management FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_computer_management TO ternilo_runtime;
END IF;
END $$;
