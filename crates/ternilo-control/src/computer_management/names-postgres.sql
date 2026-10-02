ALTER TABLE control_computer_names ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_computer_names_scope ON control_computer_names
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON control_computer_names FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_computer_names TO ternilo_runtime;
END IF;
END $$;
