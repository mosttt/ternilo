CREATE TRIGGER resource_live_ownership AFTER INSERT OR UPDATE OR DELETE ON control_resource_ownership
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

CREATE FUNCTION ternilo_cleanup_cloud_ownership() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
BEGIN
    DELETE FROM public.control_resource_ownership
    WHERE tenant_id=OLD.tenant_id AND resource_kind='session' AND resource_id=OLD.session_id;
    RETURN OLD;
END;
$$;
REVOKE ALL ON FUNCTION ternilo_cleanup_cloud_ownership() FROM PUBLIC;
CREATE TRIGGER resource_ownership_cloud_deleted AFTER DELETE ON cloud_sessions
FOR EACH ROW EXECUTE FUNCTION ternilo_cleanup_cloud_ownership();
