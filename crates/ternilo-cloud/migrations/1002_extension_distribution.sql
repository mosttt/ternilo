CREATE FUNCTION ternilo_cloud_extension_package_for_worker(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
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
    FROM cloud_runs AS job
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = job.tenant_id AND writer.session_id = job.session_id
    JOIN control_extension_packages AS plugin
      ON plugin.tenant_id = job.tenant_id
    JOIN control_extension_publishers AS publisher
      ON publisher.tenant_id = plugin.tenant_id
     AND publisher.key_id = plugin.publisher_key_id
    WHERE job.tenant_id = p_tenant_id AND job.run_id = p_run_id
      AND job.state IN ('running', 'cancel_requested')
      AND job.lease_owner = p_worker_id AND job.lease_token = p_lease_token
      AND job.session_fencing_token = p_fencing_token
      AND job.lease_expires_at_ms > p_now_ms
      AND writer.run_id = p_run_id AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_fencing_token
      AND writer.expires_at_ms > p_now_ms
      AND plugin.package_id = p_package_id AND plugin.version = p_version
      AND plugin.enabled AND NOT plugin.revoked AND NOT publisher.revoked
$$;

CREATE FUNCTION ternilo_cloud_extension_package_active_for_worker(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_package_id TEXT,
    p_version TEXT,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT EXISTS(
        SELECT 1
        FROM ternilo_cloud_extension_package_for_worker(
            p_tenant_id, p_run_id, p_worker_id, p_lease_token,
            p_fencing_token, p_package_id, p_version, p_now_ms
        )
    )
$$;

REVOKE ALL ON FUNCTION ternilo_cloud_extension_package_for_worker(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_extension_package_active_for_worker(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, BIGINT
) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_cloud_extension_package_for_worker(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_extension_package_active_for_worker(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, BIGINT
        ) TO ternilo_worker;
    END IF;
END;
$$;
