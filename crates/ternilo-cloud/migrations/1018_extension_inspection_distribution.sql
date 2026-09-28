CREATE FUNCTION ternilo_cloud_extension_package_for_worker_command(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_tenant_id TEXT,
    p_command_id TEXT,
    p_package_id TEXT,
    p_version TEXT,
    p_now_ms BIGINT
) RETURNS TABLE (trust JSONB, install_request JSONB)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT publisher.trust, plugin.install_request
    FROM cloud_workers AS worker
    JOIN cloud_session_commands AS command
      ON command.lease_owner = worker.worker_id
     AND command.worker_generation = worker.generation
    JOIN cloud_sessions AS session
      ON session.tenant_id = command.tenant_id
     AND session.user_id = command.user_id
     AND session.session_id = command.session_id
    JOIN control_extension_packages AS plugin
      ON plugin.tenant_id = session.tenant_id
    JOIN control_extension_publishers AS publisher
      ON publisher.tenant_id = plugin.tenant_id
     AND publisher.key_id = plugin.publisher_key_id
    WHERE worker.worker_id = p_worker_id
      AND worker.instance_nonce = p_instance_nonce
      AND worker.generation = p_generation
      AND worker.lease_expires_at_ms > p_now_ms
      AND command.tenant_id = p_tenant_id
      AND command.command_id = p_command_id
      AND command.state = 'inflight'
      AND command.read_only
      AND command.dispatch_lease_until_ms > p_now_ms
      AND plugin.package_id = p_package_id
      AND plugin.version = p_version
      AND plugin.enabled
      AND NOT plugin.revoked
      AND NOT publisher.revoked
$$;

REVOKE ALL ON FUNCTION ternilo_cloud_extension_package_for_worker_command(
    TEXT, TEXT, BIGINT, TEXT, TEXT, TEXT, TEXT, BIGINT
) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_cloud_extension_package_for_worker_command(
            TEXT, TEXT, BIGINT, TEXT, TEXT, TEXT, TEXT, BIGINT
        ) TO ternilo_worker;
    END IF;
END;
$$;
