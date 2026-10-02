ALTER TABLE control_computer_model_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_computer_model_attempts ENABLE ROW LEVEL SECURITY;
CREATE POLICY computer_model_requests_scope ON control_computer_model_requests
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
CREATE POLICY computer_model_attempts_scope ON control_computer_model_attempts
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON control_computer_model_requests,control_computer_model_attempts FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_computer_model_requests,control_computer_model_attempts TO ternilo_runtime;
END IF;
END $$;
