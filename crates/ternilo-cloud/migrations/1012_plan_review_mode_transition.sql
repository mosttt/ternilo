DROP FUNCTION ternilo_finish_cloud_run(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, JSONB, BIGINT
);

CREATE FUNCTION ternilo_finish_cloud_run(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_terminal_state TEXT,
    p_outcome JSONB,
    p_error JSONB,
    p_approved_plan_exit BOOLEAN,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    job cloud_runs%ROWTYPE;
    actual_tokens BIGINT;
    next_run_id TEXT;
BEGIN
    IF p_terminal_state NOT IN ('succeeded', 'failed', 'cancelled', 'indeterminate') THEN
        RETURN FALSE;
    END IF;
    SELECT * INTO job
    FROM cloud_runs
    WHERE tenant_id = p_tenant_id AND run_id = p_run_id
      AND state IN ('running', 'cancel_requested')
      AND lease_owner = p_worker_id AND lease_token = p_lease_token
      AND session_fencing_token = p_fencing_token
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM cloud_session_writer_leases
        WHERE tenant_id = p_tenant_id AND session_id = job.session_id
          AND run_id = p_run_id AND lease_owner = p_worker_id
          AND fencing_token = p_fencing_token
    ) THEN
        RETURN FALSE;
    END IF;

    actual_tokens := ternilo_settle_cloud_run_usage(
        p_tenant_id, p_run_id, job.quota_reservation_id, p_now_ms
    );
    IF actual_tokens IS NULL THEN
        RETURN FALSE;
    END IF;

    UPDATE cloud_runs
    SET state = p_terminal_state, outcome = p_outcome, error = p_error,
        finished_at_ms = p_now_ms, updated_at_ms = p_now_ms,
        lease_expires_at_ms = NULL
    WHERE tenant_id = p_tenant_id AND run_id = p_run_id;
    DELETE FROM cloud_session_writer_leases
    WHERE tenant_id = p_tenant_id AND session_id = job.session_id
      AND fencing_token = p_fencing_token;
    PERFORM 1 FROM cloud_sessions
    WHERE tenant_id = p_tenant_id AND session_id = job.session_id
    FOR UPDATE;
    next_run_id := ternilo_complete_cloud_session_submission(
        p_tenant_id, p_run_id, p_now_ms
    );
    UPDATE cloud_sessions
    SET state = CASE WHEN next_run_id IS NULL THEN p_terminal_state ELSE 'queued' END,
        current_run_id = NULL,
        title = CASE
            WHEN p_terminal_state = 'succeeded'
             AND title = 'New session'
             AND NULLIF(BTRIM(p_outcome->>'generated_title'), '') IS NOT NULL
            THEN LEFT(BTRIM(p_outcome->>'generated_title'), 256)
            ELSE title
        END,
        mode = CASE
            WHEN p_terminal_state = 'succeeded'
             AND p_approved_plan_exit IS TRUE
             AND mode = 'plan'
            THEN 'execute'
            ELSE mode
        END,
        updated_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND session_id = job.session_id;
    RETURN TRUE;
END;
$$;

REVOKE ALL ON FUNCTION ternilo_finish_cloud_run(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, JSONB, BOOLEAN, BIGINT
) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_finish_cloud_run(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, JSONB, BOOLEAN, BIGINT
        ) TO ternilo_worker;
    END IF;
END;
$$;
