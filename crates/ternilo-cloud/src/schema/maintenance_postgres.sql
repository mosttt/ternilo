CREATE FUNCTION ternilo_execution_maintenance() RETURNS TABLE (claims_paused BIGINT, active_runs BIGINT, active_commands BIGINT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=public,pg_temp AS $$
    SELECT runtime.claims_paused,
        (SELECT COUNT(*) FROM cloud_runs WHERE state IN ('leased','running','cancel_requested')),
        (SELECT COUNT(*) FROM cloud_session_commands WHERE state='inflight')
    FROM cloud_runtime_control runtime WHERE singleton=1
$$;

CREATE FUNCTION ternilo_pause_execution(paused BIGINT) RETURNS BIGINT
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path=public,pg_temp AS $$
    UPDATE cloud_runtime_control SET claims_paused=paused
    WHERE singleton=1 AND paused IN (0,1) RETURNING claims_paused
$$;

REVOKE ALL ON FUNCTION ternilo_execution_maintenance(), ternilo_pause_execution(BIGINT) FROM PUBLIC;
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
        GRANT EXECUTE ON FUNCTION ternilo_execution_maintenance(), ternilo_pause_execution(BIGINT) TO ternilo_runtime;
    END IF;
END $$;
