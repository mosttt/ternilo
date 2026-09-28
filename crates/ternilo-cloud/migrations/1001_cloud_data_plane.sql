CREATE TABLE cloud_runtime_control (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    claims_paused BOOLEAN NOT NULL DEFAULT FALSE
);

INSERT INTO cloud_runtime_control (singleton, claims_paused)
VALUES (TRUE, FALSE);

REVOKE ALL ON cloud_runtime_control FROM PUBLIC;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
        REVOKE ALL ON cloud_runtime_control FROM ternilo_runtime;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        REVOKE ALL ON cloud_runtime_control FROM ternilo_worker;
    END IF;
END;
$$;

CREATE TABLE cloud_runs (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    project_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    spec JSONB NOT NULL,
    spec_digest BYTEA NOT NULL CHECK (octet_length(spec_digest) = 32),
    quota_reservation_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'queued', 'leased', 'running', 'cancel_requested',
        'succeeded', 'failed', 'cancelled', 'indeterminate'
    )),
    priority INTEGER NOT NULL DEFAULT 0 CHECK (priority BETWEEN -1000 AND 1000),
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    max_attempts INTEGER NOT NULL DEFAULT 1 CHECK (max_attempts BETWEEN 1 AND 10),
    available_at_ms BIGINT NOT NULL CHECK (available_at_ms >= 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    lease_owner TEXT,
    lease_token BIGINT NOT NULL DEFAULT 0 CHECK (lease_token >= 0),
    lease_expires_at_ms BIGINT,
    session_fencing_token BIGINT,
    started_at_ms BIGINT,
    finished_at_ms BIGINT,
    cancel_requested_at_ms BIGINT,
    outcome JSONB,
    error JSONB,
    PRIMARY KEY (tenant_id, run_id),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id),
    FOREIGN KEY (tenant_id, workspace_id, project_id)
        REFERENCES control_workspaces(tenant_id, workspace_id, project_id),
    FOREIGN KEY (tenant_id, quota_reservation_id)
        REFERENCES control_quota_reservations(tenant_id, reservation_id)
);

CREATE INDEX cloud_runs_dispatch
ON cloud_runs (state, available_at_ms, priority DESC, created_at_ms)
WHERE state IN ('queued', 'leased');

CREATE INDEX cloud_runs_session
ON cloud_runs (tenant_id, session_id, created_at_ms);

CREATE INDEX cloud_runs_workspace
ON cloud_runs (tenant_id, workspace_id, created_at_ms);

CREATE TABLE cloud_sessions (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    project_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT 'New session' CHECK (length(title) BETWEEN 1 AND 256),
    permissions TEXT NOT NULL DEFAULT 'workspace_write'
        CHECK (permissions IN ('read_only', 'workspace_write')),
    route_id TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    reasoning_effort TEXT CHECK (reasoning_effort IN (
        'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'
    )),
    reserved_model_tokens BIGINT NOT NULL DEFAULT 32768 CHECK (reserved_model_tokens > 0),
    agent_preset TEXT NOT NULL DEFAULT 'standard',
    profile_plugins JSONB NOT NULL DEFAULT '[]'::jsonb,
    mode TEXT NOT NULL DEFAULT 'execute' CHECK (mode IN ('execute', 'plan')),
    state TEXT NOT NULL CHECK (state IN (
        'idle', 'queued', 'running', 'succeeded', 'failed', 'cancelled', 'indeterminate'
    )),
    current_run_id TEXT,
    last_seq BIGINT NOT NULL DEFAULT -1 CHECK (last_seq >= -1),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, session_id),
    UNIQUE (tenant_id, session_id, user_id),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id),
    FOREIGN KEY (tenant_id, workspace_id, project_id)
        REFERENCES control_workspaces(tenant_id, workspace_id, project_id),
    FOREIGN KEY (tenant_id, current_run_id) REFERENCES cloud_runs(tenant_id, run_id)
);

CREATE TABLE cloud_session_inboxes (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    paused BOOLEAN NOT NULL DEFAULT FALSE,
    error TEXT,
    next_position BIGINT NOT NULL DEFAULT 0 CHECK (next_position >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (tenant_id, user_id, session_id),
    FOREIGN KEY (tenant_id, session_id, user_id)
        REFERENCES cloud_sessions(tenant_id, session_id, user_id) ON DELETE CASCADE
);

CREATE TABLE cloud_session_submissions (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    submission_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    requested_delivery TEXT NOT NULL CHECK (requested_delivery IN ('queue', 'steer')),
    content JSONB NOT NULL,
    submission_references JSONB NOT NULL DEFAULT '[]'::jsonb,
    attachments JSONB NOT NULL DEFAULT '[]'::jsonb,
    placement TEXT NOT NULL CHECK (placement IN ('queued', 'steering', 'running')),
    fifo_position BIGINT NOT NULL CHECK (fifo_position >= 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, session_id, submission_id),
    UNIQUE (tenant_id, run_id),
    UNIQUE (tenant_id, user_id, session_id, fifo_position),
    FOREIGN KEY (tenant_id, user_id, session_id)
        REFERENCES cloud_session_inboxes(tenant_id, user_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, run_id)
        REFERENCES cloud_runs(tenant_id, run_id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX cloud_session_submissions_running
ON cloud_session_submissions (tenant_id, user_id, session_id)
WHERE placement = 'running';

CREATE INDEX cloud_session_submissions_fifo
ON cloud_session_submissions (tenant_id, user_id, session_id, fifo_position);

CREATE TABLE cloud_session_writer_leases (
    tenant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    lease_owner TEXT NOT NULL,
    fencing_token BIGINT NOT NULL CHECK (fencing_token > 0),
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms >= 0),
    renewed_at_ms BIGINT NOT NULL CHECK (renewed_at_ms >= 0),
    PRIMARY KEY (tenant_id, session_id),
    FOREIGN KEY (tenant_id, session_id)
        REFERENCES cloud_sessions(tenant_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, run_id)
        REFERENCES cloud_runs(tenant_id, run_id) ON DELETE CASCADE
);

CREATE TABLE cloud_session_events (
    tenant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    seq BIGINT NOT NULL CHECK (seq >= 0),
    run_id TEXT NOT NULL,
    event JSONB NOT NULL,
    writer_fencing_token BIGINT NOT NULL CHECK (writer_fencing_token > 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (tenant_id, session_id, seq),
    FOREIGN KEY (tenant_id, session_id)
        REFERENCES cloud_sessions(tenant_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, run_id)
        REFERENCES cloud_runs(tenant_id, run_id) ON DELETE CASCADE
);

CREATE FUNCTION ternilo_promote_cloud_session_head(
    p_tenant_id TEXT,
    p_user_id TEXT,
    p_session_id TEXT,
    p_now_ms BIGINT
) RETURNS TEXT
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    inbox_paused BOOLEAN;
    promoted_submission_id TEXT;
    promoted_run_id TEXT;
BEGIN
    SELECT paused INTO inbox_paused
    FROM cloud_session_inboxes
    WHERE tenant_id = p_tenant_id AND user_id = p_user_id
      AND session_id = p_session_id
    FOR UPDATE;
    IF NOT FOUND OR inbox_paused OR EXISTS (
        SELECT 1
        FROM cloud_session_submissions
        WHERE tenant_id = p_tenant_id AND user_id = p_user_id
          AND session_id = p_session_id AND placement = 'running'
    ) THEN
        RETURN NULL;
    END IF;

    SELECT submission_id INTO promoted_submission_id
    FROM cloud_session_submissions
    WHERE tenant_id = p_tenant_id AND user_id = p_user_id
      AND session_id = p_session_id AND placement = 'queued'
    ORDER BY fifo_position
    FOR UPDATE
    LIMIT 1;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    UPDATE cloud_session_submissions
    SET placement = 'running', updated_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND user_id = p_user_id
      AND session_id = p_session_id AND submission_id = promoted_submission_id
    RETURNING run_id INTO promoted_run_id;
    RETURN promoted_run_id;
END;
$$;

CREATE FUNCTION ternilo_complete_cloud_session_submission(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_now_ms BIGINT
) RETURNS TEXT
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    item cloud_session_submissions%ROWTYPE;
BEGIN
    DELETE FROM cloud_session_submissions
    WHERE tenant_id = p_tenant_id AND run_id = p_run_id
    RETURNING * INTO item;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    RETURN ternilo_promote_cloud_session_head(
        item.tenant_id, item.user_id, item.session_id, p_now_ms
    );
END;
$$;

CREATE FUNCTION ternilo_claim_cloud_run(
    p_worker_id TEXT,
    p_now_ms BIGINT,
    p_lease_expires_at_ms BIGINT
) RETURNS TABLE (
    tenant_id TEXT,
    run_id TEXT,
    session_id TEXT,
    lease_token BIGINT,
    spec JSONB,
    spec_digest BYTEA
)
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    WITH claim_gate AS MATERIALIZED (
        SELECT claims_paused
        FROM cloud_runtime_control
        WHERE singleton
        FOR SHARE
    ), candidate AS (
        SELECT queued.tenant_id, queued.run_id
        FROM cloud_runs AS queued
        CROSS JOIN claim_gate
        JOIN cloud_session_submissions AS submission
          ON submission.tenant_id = queued.tenant_id
         AND submission.run_id = queued.run_id
         AND submission.placement = 'running'
        JOIN cloud_session_inboxes AS inbox
          ON inbox.tenant_id = submission.tenant_id
         AND inbox.user_id = submission.user_id
         AND inbox.session_id = submission.session_id
         AND NOT inbox.paused
        WHERE NOT claim_gate.claims_paused
          AND ((
                queued.state = 'queued'
                AND queued.available_at_ms <= p_now_ms
              ) OR (
                queued.state = 'leased'
                AND queued.lease_expires_at_ms <= p_now_ms
                AND queued.attempt < queued.max_attempts
              ))
        ORDER BY queued.priority DESC, queued.available_at_ms, queued.created_at_ms
        FOR UPDATE SKIP LOCKED
        LIMIT 1
    )
    UPDATE cloud_runs AS claimed
    SET state = 'leased',
        attempt = claimed.attempt + 1,
        lease_owner = p_worker_id,
        lease_token = claimed.lease_token + 1,
        lease_expires_at_ms = p_lease_expires_at_ms,
        updated_at_ms = p_now_ms
    FROM candidate
    WHERE claimed.tenant_id = candidate.tenant_id
      AND claimed.run_id = candidate.run_id
    RETURNING claimed.tenant_id, claimed.run_id, claimed.session_id,
              claimed.lease_token, claimed.spec, claimed.spec_digest
$$;

CREATE FUNCTION ternilo_start_cloud_run(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_now_ms BIGINT,
    p_lease_expires_at_ms BIGINT
) RETURNS TABLE (fencing_token BIGINT)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    job cloud_runs%ROWTYPE;
    acquired_fence BIGINT;
BEGIN
    SELECT * INTO job
    FROM cloud_runs
    WHERE cloud_runs.tenant_id = p_tenant_id AND cloud_runs.run_id = p_run_id
    FOR UPDATE;

    IF NOT FOUND OR job.state != 'leased' OR job.lease_owner != p_worker_id
       OR job.lease_token != p_lease_token OR job.lease_expires_at_ms <= p_now_ms THEN
        RETURN;
    END IF;

    INSERT INTO cloud_session_writer_leases
        (tenant_id, session_id, run_id, lease_owner, fencing_token,
         expires_at_ms, renewed_at_ms)
    VALUES
        (job.tenant_id, job.session_id, job.run_id, p_worker_id, 1,
         p_lease_expires_at_ms, p_now_ms)
    ON CONFLICT (tenant_id, session_id) DO UPDATE
    SET run_id = EXCLUDED.run_id,
        lease_owner = EXCLUDED.lease_owner,
        fencing_token = cloud_session_writer_leases.fencing_token + 1,
        expires_at_ms = EXCLUDED.expires_at_ms,
        renewed_at_ms = EXCLUDED.renewed_at_ms
    WHERE cloud_session_writer_leases.expires_at_ms <= p_now_ms
    RETURNING cloud_session_writer_leases.fencing_token INTO acquired_fence;

    IF acquired_fence IS NULL THEN
        RETURN;
    END IF;

    UPDATE cloud_runs
    SET state = 'running', session_fencing_token = acquired_fence,
        lease_expires_at_ms = p_lease_expires_at_ms,
        started_at_ms = COALESCE(started_at_ms, p_now_ms), updated_at_ms = p_now_ms
    WHERE cloud_runs.tenant_id = p_tenant_id AND cloud_runs.run_id = p_run_id;

    UPDATE cloud_sessions
    SET state = 'running', current_run_id = p_run_id, updated_at_ms = p_now_ms
    WHERE cloud_sessions.tenant_id = p_tenant_id
      AND cloud_sessions.session_id = job.session_id;

    RETURN QUERY SELECT acquired_fence;
END;
$$;

CREATE FUNCTION ternilo_release_cloud_claim(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    WITH released AS (
        UPDATE cloud_runs
        SET state = 'queued', lease_owner = NULL, lease_expires_at_ms = NULL,
            available_at_ms = p_now_ms, updated_at_ms = p_now_ms
        WHERE tenant_id = p_tenant_id AND run_id = p_run_id
          AND state = 'leased' AND lease_owner = p_worker_id
          AND lease_token = p_lease_token
        RETURNING 1
    )
    SELECT EXISTS(SELECT 1 FROM released)
$$;

CREATE FUNCTION ternilo_renew_cloud_run(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_now_ms BIGINT,
    p_lease_expires_at_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    session_id_value TEXT;
    changed BIGINT;
BEGIN
    SELECT session_id INTO session_id_value
    FROM cloud_runs
    WHERE tenant_id = p_tenant_id AND run_id = p_run_id
      AND state IN ('running', 'cancel_requested')
      AND lease_owner = p_worker_id AND lease_token = p_lease_token
      AND session_fencing_token = p_fencing_token
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    UPDATE cloud_session_writer_leases
    SET expires_at_ms = p_lease_expires_at_ms, renewed_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND session_id = session_id_value
      AND run_id = p_run_id AND lease_owner = p_worker_id
      AND fencing_token = p_fencing_token;
    GET DIAGNOSTICS changed = ROW_COUNT;
    IF changed != 1 THEN
        RETURN FALSE;
    END IF;

    UPDATE cloud_runs
    SET lease_expires_at_ms = p_lease_expires_at_ms, updated_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND run_id = p_run_id;
    RETURN TRUE;
END;
$$;

CREATE FUNCTION ternilo_cloud_cancel_requested(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT
) RETURNS BOOLEAN
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT EXISTS(
        SELECT 1 FROM cloud_runs
        WHERE tenant_id = p_tenant_id AND run_id = p_run_id
          AND state = 'cancel_requested' AND lease_owner = p_worker_id
          AND lease_token = p_lease_token
    )
$$;

CREATE FUNCTION ternilo_cloud_events_for_worker(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT
) RETURNS TABLE (event JSONB)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT stored.event
    FROM cloud_runs AS job
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = job.tenant_id AND writer.session_id = job.session_id
    JOIN cloud_session_events AS stored
      ON stored.tenant_id = job.tenant_id AND stored.session_id = job.session_id
    WHERE job.tenant_id = p_tenant_id AND job.run_id = p_run_id
      AND job.lease_owner = p_worker_id AND job.lease_token = p_lease_token
      AND writer.run_id = p_run_id AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_fencing_token
    ORDER BY stored.seq
$$;

CREATE FUNCTION ternilo_append_cloud_event(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_seq BIGINT,
    p_event JSONB,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    session_id_value TEXT;
    current_last_seq BIGINT;
    existing_event JSONB;
BEGIN
    SELECT job.session_id INTO session_id_value
    FROM cloud_runs AS job
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = job.tenant_id AND writer.session_id = job.session_id
    WHERE job.tenant_id = p_tenant_id AND job.run_id = p_run_id
      AND job.state IN ('running', 'cancel_requested')
      AND job.lease_owner = p_worker_id AND job.lease_token = p_lease_token
      AND job.session_fencing_token = p_fencing_token
      AND writer.run_id = p_run_id AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_fencing_token
      AND writer.expires_at_ms > p_now_ms;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    SELECT cloud_session_events.event INTO existing_event
    FROM cloud_session_events
    WHERE tenant_id = p_tenant_id AND session_id = session_id_value AND seq = p_seq;
    IF FOUND THEN
        RETURN existing_event = p_event;
    END IF;

    SELECT last_seq INTO current_last_seq
    FROM cloud_sessions
    WHERE tenant_id = p_tenant_id AND session_id = session_id_value
    FOR UPDATE;
    IF current_last_seq + 1 != p_seq THEN
        RETURN FALSE;
    END IF;

    INSERT INTO cloud_session_events
        (tenant_id, session_id, seq, run_id, event, writer_fencing_token, created_at_ms)
    VALUES
        (p_tenant_id, session_id_value, p_seq, p_run_id, p_event,
         p_fencing_token, p_now_ms);
    UPDATE cloud_sessions
    SET last_seq = p_seq,
        title = CASE
            WHEN title = 'New session'
             AND p_event->>'type' = 'session_title_generated'
             AND NULLIF(BTRIM(p_event->>'title'), '') IS NOT NULL
            THEN LEFT(BTRIM(p_event->>'title'), 256)
            ELSE title
        END,
        updated_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND session_id = session_id_value;
    RETURN TRUE;
END;
$$;

CREATE FUNCTION ternilo_finish_cloud_run(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_terminal_state TEXT,
    p_outcome JSONB,
    p_error JSONB,
    p_actual_model_tokens BIGINT,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    job cloud_runs%ROWTYPE;
    reserved_tokens BIGINT;
    next_run_id TEXT;
BEGIN
    IF p_terminal_state NOT IN ('succeeded', 'failed', 'cancelled', 'indeterminate')
       OR p_actual_model_tokens < 0 THEN
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

    IF p_terminal_state = 'succeeded' THEN
        SELECT reserved_model_tokens INTO reserved_tokens
        FROM control_quota_reservations
        WHERE tenant_id = p_tenant_id AND reservation_id = job.quota_reservation_id
          AND state = 'active'
        FOR UPDATE;
        IF NOT FOUND OR p_actual_model_tokens > reserved_tokens THEN
            RETURN FALSE;
        END IF;
        UPDATE control_quota_reservations
        SET state = 'committed', committed_model_tokens = p_actual_model_tokens
        WHERE tenant_id = p_tenant_id AND reservation_id = job.quota_reservation_id;
        UPDATE control_quota_usage
        SET used_model_tokens = used_model_tokens + p_actual_model_tokens
        WHERE tenant_id = p_tenant_id
          AND period_start = date_trunc(
              'month', to_timestamp(p_now_ms::double precision / 1000)
          )::date;
    ELSE
        UPDATE control_quota_reservations
        SET state = 'released'
        WHERE tenant_id = p_tenant_id AND reservation_id = job.quota_reservation_id
          AND state = 'active';
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
        updated_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND session_id = job.session_id;
    RETURN TRUE;
END;
$$;

CREATE FUNCTION ternilo_reap_cloud_runs(p_now_ms BIGINT) RETURNS INTEGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    job RECORD;
    next_run_id TEXT;
    indeterminate_count INTEGER;
    failed_count INTEGER;
BEGIN
    WITH expired AS (
        UPDATE cloud_runs
        SET state = 'indeterminate', finished_at_ms = p_now_ms, updated_at_ms = p_now_ms,
            error = jsonb_build_object(
                'code', 'execution',
                'message', 'worker lease expired while the run was executing'
            )
        WHERE state IN ('running', 'cancel_requested')
          AND lease_expires_at_ms <= p_now_ms
        RETURNING tenant_id, session_id, quota_reservation_id
    ), released AS (
        UPDATE control_quota_reservations AS reservation
        SET state = 'released'
        FROM expired
        WHERE reservation.tenant_id = expired.tenant_id
          AND reservation.reservation_id = expired.quota_reservation_id
          AND reservation.state = 'active'
    )
    SELECT COUNT(*)::integer INTO indeterminate_count FROM expired;

    WITH exhausted AS (
        UPDATE cloud_runs
        SET state = 'failed', finished_at_ms = p_now_ms, updated_at_ms = p_now_ms,
            error = jsonb_build_object(
                'code', 'execution',
                'message', 'worker claim lease expired and retry budget was exhausted'
            )
        WHERE state = 'leased' AND lease_expires_at_ms <= p_now_ms
          AND attempt >= max_attempts
        RETURNING tenant_id, session_id, quota_reservation_id
    ), released AS (
        UPDATE control_quota_reservations AS reservation
        SET state = 'released'
        FROM exhausted
        WHERE reservation.tenant_id = exhausted.tenant_id
          AND reservation.reservation_id = exhausted.quota_reservation_id
          AND reservation.state = 'active'
    )
    SELECT COUNT(*)::integer INTO failed_count FROM exhausted;

    DELETE FROM cloud_session_writer_leases WHERE expires_at_ms <= p_now_ms;
    FOR job IN
        SELECT tenant_id, run_id, session_id, state
        FROM cloud_runs
        WHERE finished_at_ms = p_now_ms AND state IN ('failed', 'indeterminate')
    LOOP
        PERFORM 1 FROM cloud_sessions
        WHERE tenant_id = job.tenant_id AND session_id = job.session_id
        FOR UPDATE;
        next_run_id := ternilo_complete_cloud_session_submission(
            job.tenant_id, job.run_id, p_now_ms
        );
        UPDATE cloud_sessions
        SET state = CASE WHEN next_run_id IS NULL THEN job.state ELSE 'queued' END,
            current_run_id = NULL,
            updated_at_ms = p_now_ms
        WHERE tenant_id = job.tenant_id AND session_id = job.session_id;
    END LOOP;
    RETURN indeterminate_count + failed_count;
END;
$$;

ALTER TABLE cloud_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_inboxes ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_submissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_writer_leases ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_session_events ENABLE ROW LEVEL SECURITY;

CREATE POLICY cloud_runs_scope ON cloud_runs
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY cloud_sessions_scope ON cloud_sessions
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY cloud_session_inboxes_owner_scope ON cloud_session_inboxes
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_session_submissions_owner_scope ON cloud_session_submissions
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_session_writer_leases_scope ON cloud_session_writer_leases
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY cloud_session_events_scope ON cloud_session_events
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

REVOKE ALL ON FUNCTION ternilo_claim_cloud_run(TEXT, BIGINT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_promote_cloud_session_head(TEXT, TEXT, TEXT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_complete_cloud_session_submission(TEXT, TEXT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_start_cloud_run(TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_release_cloud_claim(TEXT, TEXT, TEXT, BIGINT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_renew_cloud_run(TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_cancel_requested(TEXT, TEXT, TEXT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_events_for_worker(TEXT, TEXT, TEXT, BIGINT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_append_cloud_event(TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT, JSONB, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_finish_cloud_run(TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, JSONB, BIGINT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_reap_cloud_runs(BIGINT) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_claim_cloud_run(TEXT, BIGINT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_start_cloud_run(TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_release_cloud_claim(TEXT, TEXT, TEXT, BIGINT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_renew_cloud_run(TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_cancel_requested(TEXT, TEXT, TEXT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_events_for_worker(TEXT, TEXT, TEXT, BIGINT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_append_cloud_event(TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT, JSONB, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_finish_cloud_run(TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, JSONB, BIGINT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_reap_cloud_runs(BIGINT)
            TO ternilo_worker;
    END IF;
END;
$$;
