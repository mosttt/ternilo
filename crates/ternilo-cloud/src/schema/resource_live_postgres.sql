-- Runtime roles invoke this fixed trigger through authorized table mutations.
-- The global feed contains identifiers, never resource contents or permissions.
CREATE FUNCTION ternilo_notify_resource_live_change()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
DECLARE
    affected_tenant TEXT;
    change_sequence BIGINT;
BEGIN
    affected_tenant := CASE WHEN TG_OP = 'DELETE' THEN OLD.tenant_id ELSE NEW.tenant_id END;
    INSERT INTO public.cloud_live_changes (kind, tenant_id)
    VALUES ('resources', affected_tenant)
    RETURNING sequence INTO change_sequence;
    PERFORM pg_notify('ternilo_cloud_live', json_build_object(
        'sequence', change_sequence, 'kind', 'resources',
        'tenant_id', affected_tenant)::text);
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION ternilo_notify_resource_live_change() FROM PUBLIC;

CREATE TRIGGER resource_live_control_resource_shares
AFTER INSERT OR UPDATE OR DELETE ON control_resource_shares
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

CREATE TRIGGER resource_live_control_resource_group_shares
AFTER INSERT OR UPDATE OR DELETE ON control_resource_group_shares
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

CREATE TRIGGER resource_live_control_resource_fork_group_sources
AFTER INSERT OR UPDATE OR DELETE ON control_resource_fork_group_sources
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

CREATE TRIGGER resource_live_control_permission_groups
AFTER INSERT OR UPDATE OR DELETE ON control_permission_groups
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

CREATE TRIGGER resource_live_control_permission_group_members
AFTER INSERT OR UPDATE OR DELETE ON control_permission_group_members
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

CREATE TRIGGER resource_live_control_project_user_shares
AFTER INSERT OR UPDATE OR DELETE ON control_project_user_shares
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

CREATE TRIGGER resource_live_control_project_group_shares
AFTER INSERT OR UPDATE OR DELETE ON control_project_group_shares
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

CREATE TRIGGER resource_live_control_workspace_project_sharing
AFTER INSERT OR UPDATE OR DELETE ON control_workspace_project_sharing
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

CREATE TRIGGER resource_live_control_memberships
AFTER INSERT OR UPDATE OR DELETE ON control_memberships
FOR EACH ROW EXECUTE FUNCTION ternilo_notify_resource_live_change();

