ALTER TABLE control_edge_usage_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_edge_usage_scope ON control_edge_usage_events
USING(tenant_id=current_setting('ternilo.tenant_id',true))
WITH CHECK(tenant_id=current_setting('ternilo.tenant_id',true));
REVOKE ALL ON control_edge_usage_events FROM PUBLIC;
DO $$ BEGIN IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
GRANT SELECT,INSERT,UPDATE,DELETE ON control_edge_usage_events TO ternilo_runtime;
END IF; END $$;
