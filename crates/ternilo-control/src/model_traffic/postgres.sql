ALTER TABLE control_model_traffic_policy ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_model_traffic_accounts ENABLE ROW LEVEL SECURITY;
CREATE POLICY model_traffic_policy_scope ON control_model_traffic_policy
USING(current_setting('ternilo.model_service',true)='on') WITH CHECK(current_setting('ternilo.model_service',true)='on');
CREATE POLICY model_traffic_accounts_scope ON control_model_traffic_accounts
USING(current_setting('ternilo.model_service',true)='on') WITH CHECK(current_setting('ternilo.model_service',true)='on');
CREATE FUNCTION ternilo_computer_traffic_counts(p_actor TEXT,p_since BIGINT,p_active_since BIGINT,p_global BOOLEAN)
RETURNS TABLE(global_requests BIGINT,account_requests BIGINT,global_active BIGINT,account_active BIGINT,global_oldest BIGINT,account_oldest BIGINT)
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=public,pg_temp AS $$
BEGIN
    IF current_setting('ternilo.model_service',true) IS DISTINCT FROM 'on' THEN
        RAISE EXCEPTION 'model traffic scope is required';
    END IF;
    RETURN QUERY SELECT
        COUNT(*) FILTER(WHERE r.created_at_ms>=p_since),
        COUNT(*) FILTER(WHERE r.created_at_ms>=p_since AND r.actor_user_id=p_actor),
        COUNT(*) FILTER(WHERE r.state='pending' AND r.updated_at_ms>=p_active_since),
        COUNT(*) FILTER(WHERE r.state='pending' AND r.updated_at_ms>=p_active_since AND r.actor_user_id=p_actor),
        MIN(r.created_at_ms) FILTER(WHERE r.created_at_ms>=p_since),
        MIN(r.created_at_ms) FILTER(WHERE r.created_at_ms>=p_since AND r.actor_user_id=p_actor)
    FROM control_computer_model_requests r
    WHERE (p_global OR r.actor_user_id=p_actor) AND (r.created_at_ms>=p_since OR (r.state='pending' AND r.updated_at_ms>=p_active_since));
END;
$$;
REVOKE ALL ON control_model_traffic_policy,control_model_traffic_accounts FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_computer_traffic_counts(TEXT,BIGINT,BIGINT,BOOLEAN) FROM PUBLIC;
DO $$ BEGIN IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
    GRANT SELECT,INSERT,UPDATE,DELETE ON control_model_traffic_policy,control_model_traffic_accounts TO ternilo_runtime;
    GRANT EXECUTE ON FUNCTION ternilo_computer_traffic_counts(TEXT,BIGINT,BIGINT,BOOLEAN) TO ternilo_runtime;
END IF; END $$;
CREATE FUNCTION ternilo_model_traffic_targets(p_pattern TEXT,p_cursor TEXT,p_limit BIGINT)
RETURNS TABLE(user_id TEXT,username TEXT,name TEXT,kind TEXT,space_name TEXT)
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=public,pg_temp AS $$
BEGIN
    IF current_setting('ternilo.model_service',true) IS DISTINCT FROM 'on' THEN
        RAISE EXCEPTION 'model traffic scope is required';
    END IF;
    RETURN QUERY SELECT u.user_id,u.username,COALESCE(s.name,u.username),
        CASE WHEN s.service_account_id IS NULL THEN 'user'::TEXT ELSE 'service'::TEXT END,t.display_name
    FROM control_users u LEFT JOIN control_service_accounts s ON s.service_account_id=u.user_id
    LEFT JOIN control_tenants t ON t.tenant_id=s.tenant_id
    WHERE u.status<>'removed' AND (p_cursor IS NULL OR u.user_id<p_cursor)
      AND (p_pattern IS NULL OR LOWER(u.username) LIKE p_pattern ESCAPE '!' OR LOWER(COALESCE(s.name,u.username)) LIKE p_pattern ESCAPE '!' OR LOWER(u.user_id) LIKE p_pattern ESCAPE '!')
    ORDER BY u.user_id DESC LIMIT p_limit;
END;
$$;
REVOKE ALL ON FUNCTION ternilo_model_traffic_targets(TEXT,TEXT,BIGINT) FROM PUBLIC;
DO $$ BEGIN IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
GRANT EXECUTE ON FUNCTION ternilo_model_traffic_targets(TEXT,TEXT,BIGINT) TO ternilo_runtime;
END IF; END $$;
