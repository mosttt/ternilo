CREATE FUNCTION ternilo_cloud_create_subagent_for_worker(
    p_worker_id TEXT,
    p_tenant_id TEXT,
    p_parent_run_id TEXT,
    p_parent_lease_token BIGINT,
    p_parent_fencing_token BIGINT,
    p_child_session_id TEXT,
    p_subagent_metadata JSONB,
    p_label TEXT,
    p_now_ms BIGINT
) RETURNS TEXT
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    parent cloud_sessions%ROWTYPE;
    existing cloud_sessions%ROWTYPE;
BEGIN
    SELECT session.* INTO parent
    FROM cloud_runs AS run
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = run.tenant_id
     AND writer.session_id = run.session_id
     AND writer.run_id = run.run_id
    JOIN cloud_sessions AS session
      ON session.tenant_id = run.tenant_id
     AND session.session_id = run.session_id
    WHERE run.tenant_id = p_tenant_id
      AND run.run_id = p_parent_run_id
      AND run.state IN ('running', 'cancel_requested')
      AND run.lease_owner = p_worker_id
      AND run.lease_token = p_parent_lease_token
      AND run.session_fencing_token = p_parent_fencing_token
      AND run.lease_expires_at_ms > p_now_ms
      AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_parent_fencing_token
      AND writer.expires_at_ms > p_now_ms;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'cloud parent run lease or writer fence is no longer current';
    END IF;

    SELECT * INTO existing
    FROM cloud_sessions
    WHERE tenant_id = p_tenant_id
      AND user_id = parent.user_id
      AND (
          session_id = p_child_session_id
          OR (
              parent_session_id = parent.session_id
              AND subagent_metadata->>'subagent_id' = p_subagent_metadata->>'subagent_id'
          )
      )
    LIMIT 1;
    IF FOUND THEN
        IF existing.parent_session_id IS DISTINCT FROM parent.session_id
           OR existing.subagent_metadata IS DISTINCT FROM p_subagent_metadata THEN
            RAISE EXCEPTION 'cloud Subagent Session identity is already used';
        END IF;
        RETURN existing.session_id;
    END IF;

    INSERT INTO cloud_sessions (
        tenant_id, session_id, user_id, project_id, workspace_id, agent_id,
        parent_session_id, subagent_metadata, title, permissions, route_id, model,
        reasoning_effort, reserved_model_tokens, agent_preset, profile_plugins, mode,
        state, created_at_ms, updated_at_ms
    ) VALUES (
        parent.tenant_id, p_child_session_id, parent.user_id, parent.project_id,
        parent.workspace_id, parent.agent_id, parent.session_id, p_subagent_metadata,
        p_label, parent.permissions, parent.route_id, parent.model,
        parent.reasoning_effort, parent.reserved_model_tokens, parent.agent_preset,
        parent.profile_plugins, parent.mode, 'idle', p_now_ms, p_now_ms
    );
    RETURN p_child_session_id;
END;
$$;

CREATE FUNCTION ternilo_cloud_enqueue_subagent_for_worker(
    p_worker_id TEXT,
    p_tenant_id TEXT,
    p_parent_run_id TEXT,
    p_parent_lease_token BIGINT,
    p_parent_fencing_token BIGINT,
    p_child_session_id TEXT,
    p_run_id TEXT,
    p_submission_id TEXT,
    p_reservation_id TEXT,
    p_spec JSONB,
    p_spec_digest BYTEA,
    p_content JSONB,
    p_max_attempts INTEGER,
    p_now_ms BIGINT,
    p_expires_at_ms BIGINT
) RETURNS TEXT
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    parent cloud_runs%ROWTYPE;
    child cloud_sessions%ROWTYPE;
    quota control_quotas%ROWTYPE;
    active_runs BIGINT;
    reserved_tokens BIGINT;
    used_tokens BIGINT;
    position BIGINT;
BEGIN
    SELECT run.* INTO parent
    FROM cloud_runs AS run
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = run.tenant_id
     AND writer.session_id = run.session_id
     AND writer.run_id = run.run_id
    WHERE run.tenant_id = p_tenant_id
      AND run.run_id = p_parent_run_id
      AND run.state IN ('running', 'cancel_requested')
      AND run.lease_owner = p_worker_id
      AND run.lease_token = p_parent_lease_token
      AND run.session_fencing_token = p_parent_fencing_token
      AND run.lease_expires_at_ms > p_now_ms
      AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_parent_fencing_token
      AND writer.expires_at_ms > p_now_ms;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'cloud parent run lease or writer fence is no longer current';
    END IF;

    SELECT * INTO child FROM cloud_sessions
    WHERE tenant_id = p_tenant_id
      AND session_id = p_child_session_id
      AND user_id = parent.user_id
      AND parent_session_id = parent.session_id
      AND subagent_metadata IS NOT NULL
    FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'canonical cloud Subagent Session does not exist';
    END IF;
    IF child.current_run_id IS NOT NULL OR EXISTS (
        SELECT 1 FROM cloud_runs
        WHERE tenant_id = p_tenant_id AND session_id = p_child_session_id
          AND state IN ('queued', 'leased', 'running', 'cancel_requested')
    ) THEN
        RAISE EXCEPTION 'cloud Subagent Session already has an active run';
    END IF;
    IF p_spec->'metadata'->>'tenant_id' IS DISTINCT FROM p_tenant_id
       OR p_spec->'metadata'->>'user_id' IS DISTINCT FROM parent.user_id
       OR p_spec->'metadata'->>'session_id' IS DISTINCT FROM p_child_session_id
       OR p_spec->'metadata'->>'run_id' IS DISTINCT FROM p_run_id THEN
        RAISE EXCEPTION 'cloud Subagent RunSpec does not match its canonical Session';
    END IF;

    UPDATE control_quota_reservations SET state = 'expired'
    WHERE tenant_id = p_tenant_id AND state = 'active' AND expires_at_ms <= p_now_ms;
    SELECT * INTO quota FROM control_quotas WHERE tenant_id = p_tenant_id FOR UPDATE;
    SELECT COUNT(*), COALESCE(SUM(reserved_model_tokens), 0)
      INTO active_runs, reserved_tokens
    FROM control_quota_reservations
    WHERE tenant_id = p_tenant_id AND state = 'active' AND expires_at_ms > p_now_ms;
    IF active_runs >= quota.max_concurrent_runs THEN
        RAISE EXCEPTION 'tenant concurrent-run quota is exhausted';
    END IF;
    INSERT INTO control_quota_usage (tenant_id, period_start, used_model_tokens)
    VALUES (
        p_tenant_id,
        date_trunc('month', to_timestamp(p_now_ms::double precision / 1000))::date,
        0
    )
    ON CONFLICT (tenant_id, period_start) DO UPDATE
    SET used_model_tokens = control_quota_usage.used_model_tokens
    RETURNING used_model_tokens INTO used_tokens;
    IF used_tokens + reserved_tokens + child.reserved_model_tokens > quota.monthly_model_tokens THEN
        RAISE EXCEPTION 'tenant monthly model-token quota is exhausted';
    END IF;

    INSERT INTO control_quota_reservations (
        tenant_id, reservation_id, user_id, run_id, reserved_model_tokens,
        state, created_at_ms, expires_at_ms
    ) VALUES (
        p_tenant_id, p_reservation_id, parent.user_id, p_run_id, child.reserved_model_tokens,
        'active', p_now_ms, p_expires_at_ms
    );
    INSERT INTO cloud_runs (
        tenant_id, run_id, user_id, project_id, workspace_id, agent_id, session_id,
        spec, spec_digest, quota_reservation_id, state, priority, max_attempts,
        available_at_ms, created_at_ms, updated_at_ms
    ) VALUES (
        p_tenant_id, p_run_id, parent.user_id, child.project_id, child.workspace_id,
        child.agent_id, child.session_id, p_spec, p_spec_digest, p_reservation_id,
        'queued', 0, p_max_attempts, p_now_ms, p_now_ms, p_now_ms
    );
    INSERT INTO cloud_session_inboxes (
        tenant_id, user_id, session_id, paused, error, next_position, updated_at_ms
    ) VALUES (
        p_tenant_id, parent.user_id, child.session_id, FALSE, NULL, 1, p_now_ms
    )
    ON CONFLICT (tenant_id, user_id, session_id) DO UPDATE
    SET paused = FALSE, error = NULL,
        next_position = cloud_session_inboxes.next_position + 1,
        updated_at_ms = EXCLUDED.updated_at_ms
    RETURNING next_position - 1 INTO position;
    INSERT INTO cloud_session_submissions (
        tenant_id, user_id, session_id, submission_id, run_id, requested_delivery,
        content, submission_references, attachments, placement, fifo_position,
        created_at_ms, updated_at_ms
    ) VALUES (
        p_tenant_id, parent.user_id, child.session_id, p_submission_id, p_run_id,
        'queue', p_content, '[]'::jsonb, '[]'::jsonb, 'queued', position,
        p_now_ms, p_now_ms
    );
    PERFORM ternilo_promote_cloud_session_head(
        p_tenant_id, parent.user_id, child.session_id, p_now_ms
    );
    UPDATE cloud_sessions
    SET state = 'queued', updated_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND session_id = child.session_id;
    RETURN p_run_id;
END;
$$;

CREATE FUNCTION ternilo_cloud_subagent_run_for_worker(
    p_worker_id TEXT,
    p_tenant_id TEXT,
    p_parent_run_id TEXT,
    p_parent_lease_token BIGINT,
    p_parent_fencing_token BIGINT,
    p_child_session_id TEXT,
    p_child_run_id TEXT,
    p_now_ms BIGINT
) RETURNS TABLE (state TEXT, outcome JSONB, error JSONB)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT child.state, child.outcome, child.error
    FROM cloud_runs AS parent
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = parent.tenant_id
     AND writer.session_id = parent.session_id
     AND writer.run_id = parent.run_id
    JOIN cloud_sessions AS session
      ON session.tenant_id = parent.tenant_id
     AND session.parent_session_id = parent.session_id
     AND session.session_id = p_child_session_id
     AND session.subagent_metadata IS NOT NULL
    JOIN cloud_runs AS child
      ON child.tenant_id = session.tenant_id
     AND child.session_id = session.session_id
     AND child.run_id = p_child_run_id
    WHERE parent.tenant_id = p_tenant_id
      AND parent.run_id = p_parent_run_id
      AND parent.state IN ('running', 'cancel_requested')
      AND parent.lease_owner = p_worker_id
      AND parent.lease_token = p_parent_lease_token
      AND parent.session_fencing_token = p_parent_fencing_token
      AND parent.lease_expires_at_ms > p_now_ms
      AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_parent_fencing_token
      AND writer.expires_at_ms > p_now_ms
$$;

CREATE FUNCTION ternilo_cloud_cancel_subagent_for_worker(
    p_worker_id TEXT,
    p_tenant_id TEXT,
    p_parent_run_id TEXT,
    p_parent_lease_token BIGINT,
    p_parent_fencing_token BIGINT,
    p_child_session_id TEXT,
    p_child_run_id TEXT,
    p_now_ms BIGINT
) RETURNS TEXT
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    child cloud_runs%ROWTYPE;
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM cloud_runs AS parent
        JOIN cloud_session_writer_leases AS writer
          ON writer.tenant_id = parent.tenant_id
         AND writer.session_id = parent.session_id
         AND writer.run_id = parent.run_id
        JOIN cloud_sessions AS session
          ON session.tenant_id = parent.tenant_id
         AND session.parent_session_id = parent.session_id
         AND session.session_id = p_child_session_id
         AND session.subagent_metadata IS NOT NULL
        WHERE parent.tenant_id = p_tenant_id
          AND parent.run_id = p_parent_run_id
          AND parent.state IN ('running', 'cancel_requested')
          AND parent.lease_owner = p_worker_id
          AND parent.lease_token = p_parent_lease_token
          AND parent.session_fencing_token = p_parent_fencing_token
          AND parent.lease_expires_at_ms > p_now_ms
          AND writer.lease_owner = p_worker_id
          AND writer.fencing_token = p_parent_fencing_token
          AND writer.expires_at_ms > p_now_ms
    ) THEN
        RAISE EXCEPTION 'cloud parent run lease or Subagent lineage is no longer current';
    END IF;
    SELECT * INTO child FROM cloud_runs
    WHERE tenant_id = p_tenant_id AND run_id = p_child_run_id
      AND session_id = p_child_session_id
    FOR UPDATE;
    IF child.state IN ('succeeded', 'failed', 'cancelled', 'indeterminate') THEN
        RETURN child.state;
    END IF;
    IF child.state IN ('queued', 'leased') THEN
        UPDATE cloud_runs SET state = 'cancelled', cancel_requested_at_ms = p_now_ms,
            finished_at_ms = p_now_ms, updated_at_ms = p_now_ms,
            lease_expires_at_ms = NULL
        WHERE tenant_id = p_tenant_id AND run_id = p_child_run_id;
        DELETE FROM cloud_session_submissions
        WHERE tenant_id = p_tenant_id AND run_id = p_child_run_id;
        UPDATE control_quota_reservations SET state = 'released'
        WHERE tenant_id = p_tenant_id AND reservation_id = child.quota_reservation_id
          AND state = 'active';
        UPDATE cloud_sessions SET state = 'cancelled', current_run_id = NULL,
            updated_at_ms = p_now_ms
        WHERE tenant_id = p_tenant_id AND session_id = p_child_session_id;
        RETURN 'cancelled';
    END IF;
    UPDATE cloud_runs SET state = 'cancel_requested', cancel_requested_at_ms = p_now_ms,
        updated_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND run_id = p_child_run_id;
    RETURN 'cancel_requested';
END;
$$;

REVOKE ALL ON FUNCTION ternilo_cloud_create_subagent_for_worker(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, TEXT, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_enqueue_subagent_for_worker(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, TEXT, TEXT, JSONB, BYTEA,
    JSONB, INTEGER, BIGINT, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_subagent_run_for_worker(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_cancel_subagent_for_worker(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, BIGINT
) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_cloud_create_subagent_for_worker(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, TEXT, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_enqueue_subagent_for_worker(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, TEXT, TEXT, JSONB, BYTEA,
            JSONB, INTEGER, BIGINT, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_subagent_run_for_worker(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_cancel_subagent_for_worker(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, TEXT, BIGINT
        ) TO ternilo_worker;
    END IF;
END;
$$;
