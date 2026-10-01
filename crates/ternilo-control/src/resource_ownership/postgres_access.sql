ALTER TABLE control_resource_ownership ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_resource_ownership_scope ON control_resource_ownership
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON control_resource_ownership FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_resource_ownership TO ternilo_runtime;
END IF;
END $$;

CREATE FUNCTION ternilo_cleanup_edge_ownership() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
BEGIN
    DELETE FROM public.control_resource_ownership
    WHERE tenant_id=OLD.tenant_id AND resource_kind='session' AND resource_id=OLD.session_id;
    RETURN OLD;
END;
$$;
REVOKE ALL ON FUNCTION ternilo_cleanup_edge_ownership() FROM PUBLIC;
CREATE TRIGGER resource_ownership_edge_deleted AFTER DELETE ON control_edge_sessions
FOR EACH ROW EXECUTE FUNCTION ternilo_cleanup_edge_ownership();
