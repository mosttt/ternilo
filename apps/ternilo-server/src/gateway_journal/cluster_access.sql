ALTER TABLE gateway_peer_routes ENABLE ROW LEVEL SECURITY;
CREATE POLICY gateway_peer_scope ON gateway_peer_routes
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
ALTER TABLE gateway_live_routes ENABLE ROW LEVEL SECURITY;
-- The feed exposes only routing identifiers and revisions, never payloads.
CREATE POLICY gateway_live_read ON gateway_live_routes FOR SELECT USING (true);
CREATE POLICY gateway_live_insert ON gateway_live_routes FOR INSERT
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
CREATE POLICY gateway_live_update ON gateway_live_routes FOR UPDATE
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON gateway_peer_routes, gateway_live_routes FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE ON gateway_peer_routes, gateway_live_routes TO ternilo_runtime;
END IF;
END $$;
