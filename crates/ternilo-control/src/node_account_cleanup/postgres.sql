ALTER TABLE control_node_input_authorizations ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_node_storage_bindings ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_node_storage_binding_scope ON control_node_storage_bindings
USING (tenant_id=current_setting('ternilo.tenant_id',true))
WITH CHECK (tenant_id=current_setting('ternilo.tenant_id',true));
CREATE POLICY control_node_input_authorization_scope ON control_node_input_authorizations
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
ALTER TABLE control_node_account_cleanup ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_node_account_cleanup_scope ON control_node_account_cleanup
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
CREATE FUNCTION ternilo_node_cleanup_targets(p_actor TEXT, p_user TEXT)
RETURNS TABLE (tenant_id TEXT, executor_id TEXT, credential_id TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp
AS $$
    SELECT DISTINCT c.tenant_id, c.executor_id, c.credential_id
    FROM control_node_credentials c
    JOIN control_executors e ON e.tenant_id=c.tenant_id AND e.executor_id=c.executor_id
    WHERE (e.owner_user_id=p_user OR EXISTS (
        SELECT 1 FROM control_node_input_authorizations a
        WHERE a.credential_id=c.credential_id AND a.user_id=p_user))
      AND EXISTS (SELECT 1 FROM control_users target
        WHERE target.user_id=p_user AND target.status IN ('banned','removed')
          AND target.platform_role<>'owner' AND target.user_id<>p_actor)
      AND EXISTS (SELECT 1 FROM control_users admin
        JOIN control_instance_settings instance ON instance.singleton=1
        WHERE admin.user_id=p_actor AND admin.status='active'
          AND (admin.user_id=instance.owner_user_id OR
            (instance.mode='multi_user' AND admin.platform_role='admin')))
    ORDER BY c.tenant_id, c.executor_id, c.credential_id
$$;
REVOKE ALL ON FUNCTION ternilo_node_cleanup_targets(TEXT,TEXT) FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
GRANT SELECT,INSERT,UPDATE,DELETE ON control_node_input_authorizations,control_node_account_cleanup,control_node_storage_bindings TO ternilo_runtime;
GRANT EXECUTE ON FUNCTION ternilo_node_cleanup_targets(TEXT,TEXT) TO ternilo_runtime;
END IF;
END $$;

CREATE FUNCTION ternilo_account_cleanup_records(p_actor TEXT,p_user TEXT)
RETURNS SETOF control_node_account_cleanup
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=public,pg_temp
AS $$
    SELECT cleanup.* FROM control_node_account_cleanup cleanup
    WHERE cleanup.user_id=p_user AND EXISTS (
        SELECT 1 FROM control_users actor JOIN control_instance_settings instance ON instance.singleton=1
        WHERE actor.user_id=p_actor AND actor.status='active'
          AND (actor.user_id=instance.owner_user_id OR
            (instance.mode='multi_user' AND actor.platform_role IN ('admin','auditor'))))
    ORDER BY cleanup.created_at_ms DESC,cleanup.request_id
$$;
REVOKE ALL ON FUNCTION ternilo_account_cleanup_records(TEXT,TEXT) FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
GRANT EXECUTE ON FUNCTION ternilo_account_cleanup_records(TEXT,TEXT) TO ternilo_runtime;
END IF;
END $$;
