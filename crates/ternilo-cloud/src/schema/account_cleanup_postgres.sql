-- Discover only unfinished work for an account that a platform administrator closed.
-- Mutations retain the normal tenant policies and recheck canonical state in Rust.
CREATE FUNCTION ternilo_cloud_account_run_scopes(p_admin_user_id TEXT, p_actor_user_id TEXT)
RETURNS TABLE(tenant_id TEXT, session_id TEXT, run_id TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp AS $$
    SELECT run.tenant_id, run.session_id, run.run_id
    FROM cloud_runs AS run
    WHERE run.actor_user_id = p_actor_user_id
      AND run.state IN ('queued', 'leased', 'running', 'cancel_requested')
      AND EXISTS (
          SELECT 1 FROM control_users AS administrator
          JOIN control_instance_settings AS instance ON instance.singleton = 1
          JOIN control_users AS target ON target.user_id = p_actor_user_id
          WHERE administrator.user_id = p_admin_user_id AND administrator.status = 'active'
            AND (administrator.user_id = instance.owner_user_id
                 OR (instance.mode = 'multi_user' AND administrator.platform_role = 'admin'))
            AND target.status IN ('banned', 'removed')
            AND target.user_id <> administrator.user_id
            AND target.user_id <> instance.owner_user_id
      )
    ORDER BY run.tenant_id, run.session_id, run.run_id
$$;
REVOKE ALL ON FUNCTION ternilo_cloud_account_run_scopes(TEXT, TEXT) FROM PUBLIC;
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
        GRANT EXECUTE ON FUNCTION ternilo_cloud_account_run_scopes(TEXT, TEXT) TO ternilo_runtime;
    END IF;
END $$;
