CREATE TABLE cloud_model_usage (
    tenant_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    lease_token BIGINT NOT NULL CHECK (lease_token > 0),
    request_id BIGINT NOT NULL CHECK (request_id > 0),
    route_id TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens BIGINT NOT NULL CHECK (input_tokens >= 0),
    output_tokens BIGINT NOT NULL CHECK (output_tokens >= 0),
    cached_input_tokens BIGINT NOT NULL CHECK (
        cached_input_tokens >= 0 AND cached_input_tokens <= input_tokens
    ),
    provider_request_id TEXT,
    recorded_at_ms BIGINT NOT NULL CHECK (recorded_at_ms >= 0),
    PRIMARY KEY (tenant_id, run_id, lease_token, request_id),
    FOREIGN KEY (tenant_id, run_id)
        REFERENCES cloud_runs(tenant_id, run_id) ON DELETE CASCADE
);

CREATE FUNCTION ternilo_record_cloud_model_usage(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_request_id BIGINT,
    p_route_id TEXT,
    p_model TEXT,
    p_input_tokens BIGINT,
    p_output_tokens BIGINT,
    p_cached_input_tokens BIGINT,
    p_provider_request_id TEXT,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
BEGIN
    IF p_request_id <= 0
       OR length(p_route_id) NOT BETWEEN 1 AND 64
       OR length(p_model) NOT BETWEEN 1 AND 200
       OR p_input_tokens < 0
       OR p_output_tokens < 0
       OR p_cached_input_tokens < 0
       OR p_cached_input_tokens > p_input_tokens
       OR p_now_ms < 0
       OR length(COALESCE(p_provider_request_id, '')) > 512 THEN
        RETURN FALSE;
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM cloud_runs AS run
        JOIN cloud_session_writer_leases AS writer
          ON writer.tenant_id = run.tenant_id
         AND writer.session_id = run.session_id
         AND writer.run_id = run.run_id
        WHERE run.tenant_id = p_tenant_id
          AND run.run_id = p_run_id
          AND run.state IN ('running', 'cancel_requested')
          AND run.lease_owner = p_worker_id
          AND run.lease_token = p_lease_token
          AND run.session_fencing_token = p_fencing_token
          AND writer.lease_owner = p_worker_id
          AND writer.fencing_token = p_fencing_token
    ) THEN
        RETURN FALSE;
    END IF;

    INSERT INTO cloud_model_usage
        (tenant_id, run_id, lease_token, request_id, route_id, model,
         input_tokens, output_tokens, cached_input_tokens,
         provider_request_id, recorded_at_ms)
    VALUES
        (p_tenant_id, p_run_id, p_lease_token, p_request_id, p_route_id, p_model,
         p_input_tokens, p_output_tokens, p_cached_input_tokens,
         p_provider_request_id, p_now_ms)
    ON CONFLICT DO NOTHING;
    IF FOUND THEN
        RETURN TRUE;
    END IF;

    RETURN EXISTS (
        SELECT 1 FROM cloud_model_usage
        WHERE tenant_id = p_tenant_id
          AND run_id = p_run_id
          AND lease_token = p_lease_token
          AND request_id = p_request_id
          AND route_id = p_route_id
          AND model = p_model
          AND input_tokens = p_input_tokens
          AND output_tokens = p_output_tokens
          AND cached_input_tokens = p_cached_input_tokens
          AND provider_request_id IS NOT DISTINCT FROM p_provider_request_id
    );
END;
$$;

CREATE FUNCTION ternilo_settle_cloud_run_usage(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_reservation_id TEXT,
    p_now_ms BIGINT
) RETURNS BIGINT
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    reserved_tokens BIGINT;
    actual_tokens BIGINT;
BEGIN
    SELECT reserved_model_tokens INTO reserved_tokens
    FROM control_quota_reservations
    WHERE tenant_id = p_tenant_id
      AND reservation_id = p_reservation_id
      AND state = 'active'
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    SELECT (
        COALESCE(SUM(input_tokens), 0) + COALESCE(SUM(output_tokens), 0)
    )::bigint INTO actual_tokens
    FROM cloud_model_usage
    WHERE tenant_id = p_tenant_id AND run_id = p_run_id;

    IF actual_tokens > 0 THEN
        UPDATE control_quota_reservations
        SET state = 'committed', committed_model_tokens = actual_tokens
        WHERE tenant_id = p_tenant_id AND reservation_id = p_reservation_id;
        UPDATE control_quota_usage
        SET used_model_tokens = used_model_tokens + actual_tokens
        WHERE tenant_id = p_tenant_id
          AND period_start = date_trunc(
              'month', to_timestamp(p_now_ms::double precision / 1000)
          )::date;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'quota usage row is missing for tenant %', p_tenant_id;
        END IF;
    ELSE
        UPDATE control_quota_reservations
        SET state = 'released', committed_model_tokens = NULL
        WHERE tenant_id = p_tenant_id AND reservation_id = p_reservation_id;
    END IF;
    RETURN actual_tokens;
END;
$$;

DROP FUNCTION ternilo_finish_cloud_run(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, JSONB, BIGINT, BIGINT
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
        updated_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND session_id = job.session_id;
    RETURN TRUE;
END;
$$;

CREATE OR REPLACE FUNCTION ternilo_reap_cloud_runs(p_now_ms BIGINT) RETURNS INTEGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    job RECORD;
    next_run_id TEXT;
    indeterminate_count INTEGER := 0;
    failed_count INTEGER := 0;
BEGIN
    FOR job IN
        UPDATE cloud_runs
        SET state = 'indeterminate', finished_at_ms = p_now_ms, updated_at_ms = p_now_ms,
            error = jsonb_build_object(
                'code', 'execution',
                'message', 'worker lease expired while the run was executing'
            )
        WHERE state IN ('running', 'cancel_requested')
          AND lease_expires_at_ms <= p_now_ms
        RETURNING tenant_id, run_id, session_id, quota_reservation_id
    LOOP
        PERFORM ternilo_settle_cloud_run_usage(
            job.tenant_id, job.run_id, job.quota_reservation_id, p_now_ms
        );
        PERFORM 1 FROM cloud_sessions
        WHERE tenant_id = job.tenant_id AND session_id = job.session_id
        FOR UPDATE;
        next_run_id := ternilo_complete_cloud_session_submission(
            job.tenant_id, job.run_id, p_now_ms
        );
        UPDATE cloud_sessions
        SET state = CASE WHEN next_run_id IS NULL THEN 'indeterminate' ELSE 'queued' END,
            current_run_id = NULL,
            updated_at_ms = p_now_ms
        WHERE tenant_id = job.tenant_id AND session_id = job.session_id;
        indeterminate_count := indeterminate_count + 1;
    END LOOP;

    FOR job IN
        UPDATE cloud_runs
        SET state = 'failed', finished_at_ms = p_now_ms, updated_at_ms = p_now_ms,
            error = jsonb_build_object(
                'code', 'execution',
                'message', 'worker claim lease expired and retry budget was exhausted'
            )
        WHERE state = 'leased' AND lease_expires_at_ms <= p_now_ms
          AND attempt >= max_attempts
        RETURNING tenant_id, run_id, session_id, quota_reservation_id
    LOOP
        PERFORM ternilo_settle_cloud_run_usage(
            job.tenant_id, job.run_id, job.quota_reservation_id, p_now_ms
        );
        PERFORM 1 FROM cloud_sessions
        WHERE tenant_id = job.tenant_id AND session_id = job.session_id
        FOR UPDATE;
        next_run_id := ternilo_complete_cloud_session_submission(
            job.tenant_id, job.run_id, p_now_ms
        );
        UPDATE cloud_sessions
        SET state = CASE WHEN next_run_id IS NULL THEN 'failed' ELSE 'queued' END,
            current_run_id = NULL,
            updated_at_ms = p_now_ms
        WHERE tenant_id = job.tenant_id AND session_id = job.session_id;
        failed_count := failed_count + 1;
    END LOOP;

    DELETE FROM cloud_session_writer_leases WHERE expires_at_ms <= p_now_ms;
    RETURN indeterminate_count + failed_count;
END;
$$;

ALTER TABLE cloud_model_usage ENABLE ROW LEVEL SECURITY;

CREATE POLICY cloud_model_usage_scope ON cloud_model_usage
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

REVOKE ALL ON FUNCTION ternilo_record_cloud_model_usage(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT, TEXT, TEXT,
    BIGINT, BIGINT, BIGINT, TEXT, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_settle_cloud_run_usage(TEXT, TEXT, TEXT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_finish_cloud_run(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, JSONB, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_reap_cloud_runs(BIGINT) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_record_cloud_model_usage(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT, TEXT, TEXT,
            BIGINT, BIGINT, BIGINT, TEXT, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_finish_cloud_run(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, JSONB, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_reap_cloud_runs(BIGINT)
            TO ternilo_worker;
    END IF;
END;
$$;
