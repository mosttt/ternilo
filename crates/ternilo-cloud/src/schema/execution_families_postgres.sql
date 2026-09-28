ALTER TABLE cloud_execution_families ENABLE ROW LEVEL SECURITY;
CREATE POLICY cloud_execution_families_scope ON cloud_execution_families
USING (tenant_id=current_setting('ternilo.tenant_id',true) AND owner_user_id=current_setting('ternilo.user_id',true))
WITH CHECK (tenant_id=current_setting('ternilo.tenant_id',true) AND owner_user_id=current_setting('ternilo.user_id',true));
DO $$ BEGIN IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN GRANT SELECT,INSERT ON cloud_execution_families TO ternilo_runtime; END IF; END $$;
