CREATE FUNCTION ternilo_cloud_model_budget(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT
) RETURNS TABLE (reserved_tokens BIGINT, recorded_tokens BIGINT)
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT reservation.reserved_model_tokens,
           (
               SELECT COALESCE(SUM(usage.input_tokens + usage.output_tokens), 0)::bigint
               FROM cloud_model_usage AS usage
               WHERE usage.tenant_id = run.tenant_id
                 AND usage.run_id = run.run_id
           ) AS recorded_tokens
    FROM cloud_runs AS run
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = run.tenant_id
     AND writer.session_id = run.session_id
     AND writer.run_id = run.run_id
    JOIN control_quota_reservations AS reservation
      ON reservation.tenant_id = run.tenant_id
     AND reservation.reservation_id = run.quota_reservation_id
     AND reservation.state = 'active'
    WHERE run.tenant_id = p_tenant_id
      AND run.run_id = p_run_id
      AND run.state IN ('running', 'cancel_requested')
      AND run.lease_owner = p_worker_id
      AND run.lease_token = p_lease_token
      AND run.session_fencing_token = p_fencing_token
      AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_fencing_token
$$;

REVOKE ALL ON FUNCTION ternilo_cloud_model_budget(
    TEXT, TEXT, TEXT, BIGINT, BIGINT
) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_cloud_model_budget(
            TEXT, TEXT, TEXT, BIGINT, BIGINT
        ) TO ternilo_worker;
    END IF;
END;
$$;
