ALTER TABLE cloud_session_stats ENABLE ROW LEVEL SECURITY;
CREATE POLICY cloud_session_stats_scope ON cloud_session_stats
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON cloud_session_stats FROM PUBLIC;
DO $$
BEGIN
    IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
        GRANT SELECT,INSERT,UPDATE,DELETE ON cloud_session_stats TO ternilo_runtime;
    END IF;
END;
$$;
