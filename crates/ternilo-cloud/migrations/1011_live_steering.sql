ALTER TABLE cloud_session_submissions
    ADD COLUMN steering_command_id TEXT,
    ADD COLUMN steering_target_run_id TEXT,
    ADD COLUMN steering_target_writer_fencing_token BIGINT,
    ADD CONSTRAINT cloud_session_submissions_steering_command_fkey
        FOREIGN KEY (tenant_id, steering_command_id)
        REFERENCES cloud_session_commands(tenant_id, command_id),
    ADD CONSTRAINT cloud_session_submissions_steering_target_fkey
        FOREIGN KEY (tenant_id, steering_target_run_id)
        REFERENCES cloud_runs(tenant_id, run_id),
    ADD CONSTRAINT cloud_session_submissions_steering_shape CHECK (
        (
            steering_command_id IS NULL
            AND steering_target_run_id IS NULL
            AND steering_target_writer_fencing_token IS NULL
        ) OR (
            steering_command_id IS NOT NULL
            AND steering_target_run_id IS NOT NULL
            AND steering_target_writer_fencing_token > 0
        )
    );

CREATE INDEX cloud_session_submissions_steering_command
ON cloud_session_submissions (tenant_id, steering_command_id)
WHERE steering_command_id IS NOT NULL;

CREATE FUNCTION ternilo_cloud_run_submission_for_worker(
    p_worker_id TEXT,
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_now_ms BIGINT
) RETURNS TABLE (
    submission_id TEXT,
    run_id TEXT,
    content JSONB,
    submission_references JSONB,
    attachments JSONB,
    placement TEXT,
    created_at_ms BIGINT,
    updated_at_ms BIGINT
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT submission.submission_id, submission.run_id, submission.content,
           submission.submission_references, submission.attachments,
           submission.placement, submission.created_at_ms, submission.updated_at_ms
    FROM cloud_runs AS run
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = run.tenant_id
     AND writer.session_id = run.session_id
     AND writer.run_id = run.run_id
    JOIN cloud_session_submissions AS submission
      ON submission.tenant_id = run.tenant_id
     AND submission.run_id = run.run_id
     AND submission.user_id = run.user_id
     AND submission.session_id = run.session_id
    WHERE run.tenant_id = p_tenant_id
      AND run.run_id = p_run_id
      AND run.state IN ('running', 'cancel_requested')
      AND run.lease_owner = p_worker_id
      AND run.lease_token = p_lease_token
      AND run.lease_expires_at_ms > p_now_ms
      AND run.session_fencing_token = p_fencing_token
      AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_fencing_token
      AND writer.expires_at_ms > p_now_ms
      AND submission.placement = 'running'
$$;

CREATE FUNCTION ternilo_cloud_steering_submission_for_worker(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_tenant_id TEXT,
    p_command_id TEXT,
    p_now_ms BIGINT
) RETURNS TABLE (
    submission_id TEXT,
    run_id TEXT,
    content JSONB,
    submission_references JSONB,
    attachments JSONB,
    created_at_ms BIGINT,
    updated_at_ms BIGINT
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT submission.submission_id, submission.run_id, submission.content,
           submission.submission_references, submission.attachments,
           submission.created_at_ms, submission.updated_at_ms
    FROM cloud_workers AS worker
    JOIN cloud_session_commands AS command
      ON command.tenant_id = p_tenant_id
     AND command.command_id = p_command_id
    JOIN cloud_session_submissions AS submission
      ON submission.tenant_id = command.tenant_id
     AND submission.user_id = command.user_id
     AND submission.session_id = command.session_id
     AND submission.steering_command_id = command.command_id
    JOIN cloud_runs AS active_run
      ON active_run.tenant_id = command.tenant_id
     AND active_run.run_id = command.target_run_id
     AND active_run.session_id = command.session_id
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = active_run.tenant_id
     AND writer.session_id = active_run.session_id
     AND writer.run_id = active_run.run_id
    WHERE worker.worker_id = p_worker_id
      AND worker.instance_nonce = p_instance_nonce
      AND worker.generation = p_generation
      AND worker.lease_expires_at_ms > p_now_ms
      AND command.state = 'inflight'
      AND command.lease_owner = p_worker_id
      AND command.worker_generation = p_generation
      AND command.dispatch_lease_until_ms > p_now_ms
      AND command.required_capability = 'session_steering'
      AND command.target_writer_fencing_token = active_run.session_fencing_token
      AND submission.steering_target_run_id = command.target_run_id
      AND submission.steering_target_writer_fencing_token = command.target_writer_fencing_token
      AND active_run.state IN ('running', 'cancel_requested')
      AND active_run.lease_owner = p_worker_id
      AND active_run.lease_expires_at_ms > p_now_ms
      AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = command.target_writer_fencing_token
      AND writer.expires_at_ms > p_now_ms
      AND submission.placement = 'queued'
$$;

CREATE FUNCTION ternilo_complete_cloud_steering_command(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_tenant_id TEXT,
    p_command_id TEXT,
    p_accepted BOOLEAN,
    p_reply_json JSONB,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    command_row cloud_session_commands%ROWTYPE;
    changed BIGINT;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM cloud_workers
        WHERE worker_id = p_worker_id
          AND instance_nonce = p_instance_nonce
          AND generation = p_generation
          AND lease_expires_at_ms > p_now_ms
    ) THEN
        RETURN FALSE;
    END IF;

    SELECT * INTO command_row
    FROM cloud_session_commands
    WHERE tenant_id = p_tenant_id AND command_id = p_command_id
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    IF command_row.state = 'completed' THEN
        RETURN command_row.reply_json = p_reply_json;
    END IF;
    IF command_row.state != 'inflight'
       OR command_row.lease_owner != p_worker_id
       OR command_row.worker_generation != p_generation
       OR command_row.dispatch_lease_until_ms <= p_now_ms
       OR command_row.required_capability != 'session_steering'
       OR NOT EXISTS (
            SELECT 1
            FROM cloud_runs AS run
            JOIN cloud_session_writer_leases AS writer
              ON writer.tenant_id = run.tenant_id
             AND writer.session_id = run.session_id
            WHERE run.tenant_id = command_row.tenant_id
              AND run.session_id = command_row.session_id
              AND run.run_id = command_row.target_run_id
              AND run.state IN ('running', 'cancel_requested')
              AND run.lease_owner = p_worker_id
              AND run.lease_expires_at_ms > p_now_ms
              AND run.session_fencing_token = command_row.target_writer_fencing_token
              AND writer.run_id = run.run_id
              AND writer.lease_owner = p_worker_id
              AND writer.fencing_token = command_row.target_writer_fencing_token
              AND writer.expires_at_ms > p_now_ms
       ) THEN
        RETURN FALSE;
    END IF;

    UPDATE cloud_session_submissions
    SET placement = CASE WHEN p_accepted THEN 'steering' ELSE 'queued' END,
        steering_command_id = CASE WHEN p_accepted THEN steering_command_id ELSE NULL END,
        steering_target_run_id = CASE WHEN p_accepted THEN steering_target_run_id ELSE NULL END,
        steering_target_writer_fencing_token = CASE
            WHEN p_accepted THEN steering_target_writer_fencing_token ELSE NULL
        END,
        updated_at_ms = p_now_ms
    WHERE tenant_id = command_row.tenant_id
      AND user_id = command_row.user_id
      AND session_id = command_row.session_id
      AND steering_command_id = command_row.command_id
      AND steering_target_run_id = command_row.target_run_id
      AND steering_target_writer_fencing_token = command_row.target_writer_fencing_token
      AND placement = 'queued';
    GET DIAGNOSTICS changed = ROW_COUNT;
    IF changed != 1 THEN
        RETURN FALSE;
    END IF;

    UPDATE cloud_session_commands
    SET state = 'completed',
        lease_owner = NULL,
        worker_generation = NULL,
        dispatch_lease_until_ms = NULL,
        reply_json = p_reply_json,
        completed_at_ms = p_now_ms,
        updated_at_ms = p_now_ms
    WHERE tenant_id = command_row.tenant_id
      AND command_id = command_row.command_id;
    RETURN TRUE;
END;
$$;

CREATE FUNCTION ternilo_requeue_cloud_steering_for_run(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_now_ms BIGINT
) RETURNS INTEGER
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    WITH requeued AS (
        UPDATE cloud_session_submissions
        SET placement = 'queued',
            steering_command_id = NULL,
            steering_target_run_id = NULL,
            steering_target_writer_fencing_token = NULL,
            updated_at_ms = p_now_ms
        WHERE tenant_id = p_tenant_id
          AND steering_target_run_id = p_run_id
          AND placement = 'steering'
        RETURNING 1
    )
    SELECT COUNT(*)::INTEGER FROM requeued
$$;

CREATE FUNCTION ternilo_requeue_cloud_steering_for_run(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_lease_token BIGINT,
    p_writer_fencing_token BIGINT,
    p_now_ms BIGINT
) RETURNS INTEGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM cloud_workers AS worker
        JOIN cloud_runs AS run
          ON run.tenant_id = p_tenant_id
         AND run.run_id = p_run_id
        JOIN cloud_session_writer_leases AS writer
          ON writer.tenant_id = run.tenant_id
         AND writer.session_id = run.session_id
         AND writer.run_id = run.run_id
        WHERE worker.worker_id = p_worker_id
          AND worker.instance_nonce = p_instance_nonce
          AND worker.generation = p_generation
          AND worker.lease_expires_at_ms > p_now_ms
          AND run.state IN ('running', 'cancel_requested')
          AND run.lease_owner = p_worker_id
          AND run.lease_token = p_lease_token
          AND run.lease_expires_at_ms > p_now_ms
          AND run.session_fencing_token = p_writer_fencing_token
          AND writer.lease_owner = p_worker_id
          AND writer.fencing_token = p_writer_fencing_token
          AND writer.expires_at_ms > p_now_ms
    ) THEN
        RETURN 0;
    END IF;
    RETURN ternilo_requeue_cloud_steering_for_run(p_tenant_id, p_run_id, p_now_ms);
END;
$$;

CREATE FUNCTION ternilo_consume_cloud_steering_event(
    p_tenant_id TEXT,
    p_session_id TEXT,
    p_run_id TEXT,
    p_writer_fencing_token BIGINT,
    p_event JSONB,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    source_submission_id TEXT;
    candidate_run_id TEXT;
    candidate_reservation_id TEXT;
    active_reservation_id TEXT;
    candidate_tokens BIGINT;
BEGIN
    IF COALESCE(p_event->>'type', '') != 'user_message' THEN
        RETURN TRUE;
    END IF;
    IF p_event->'source' IS NULL
       OR COALESCE(p_event#>>'{source,kind}', '') != 'submission'
       OR COALESCE(p_event#>>'{source,delivery}', '') != 'steer' THEN
        RETURN TRUE;
    END IF;
    source_submission_id := p_event#>>'{source,submission_id}';
    IF source_submission_id IS NULL THEN
        RETURN FALSE;
    END IF;

    SELECT submission.run_id, candidate.quota_reservation_id,
           active.quota_reservation_id, reservation.reserved_model_tokens
    INTO candidate_run_id, candidate_reservation_id,
         active_reservation_id, candidate_tokens
    FROM cloud_session_submissions AS submission
    JOIN cloud_runs AS candidate
      ON candidate.tenant_id = submission.tenant_id
     AND candidate.run_id = submission.run_id
    JOIN cloud_runs AS active
      ON active.tenant_id = submission.tenant_id
     AND active.run_id = submission.steering_target_run_id
    JOIN control_quota_reservations AS reservation
      ON reservation.tenant_id = candidate.tenant_id
     AND reservation.reservation_id = candidate.quota_reservation_id
    WHERE submission.tenant_id = p_tenant_id
      AND submission.session_id = p_session_id
      AND submission.submission_id = source_submission_id
      AND submission.placement = 'steering'
      AND submission.steering_target_run_id = p_run_id
      AND submission.steering_target_writer_fencing_token = p_writer_fencing_token
      AND candidate.state = 'queued'
      AND active.state IN ('running', 'cancel_requested')
      AND active.session_fencing_token = p_writer_fencing_token
      AND reservation.state = 'active'
    FOR UPDATE OF submission, candidate, active, reservation;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    UPDATE control_quota_reservations
    SET reserved_model_tokens = reserved_model_tokens + candidate_tokens
    WHERE tenant_id = p_tenant_id
      AND reservation_id = active_reservation_id
      AND state = 'active';
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    UPDATE control_quota_reservations
    SET state = 'released'
    WHERE tenant_id = p_tenant_id
      AND reservation_id = candidate_reservation_id
      AND state = 'active';
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    DELETE FROM cloud_runs
    WHERE tenant_id = p_tenant_id AND run_id = candidate_run_id;
    UPDATE cloud_session_inboxes
    SET updated_at_ms = p_now_ms
    WHERE tenant_id = p_tenant_id AND session_id = p_session_id;
    RETURN TRUE;
END;
$$;

CREATE OR REPLACE FUNCTION ternilo_append_cloud_event(
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
    IF NOT ternilo_consume_cloud_steering_event(
        p_tenant_id, session_id_value, p_run_id, p_fencing_token, p_event, p_now_ms
    ) THEN
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

CREATE OR REPLACE FUNCTION ternilo_release_cloud_session_commands(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_now_ms BIGINT
) RETURNS INTEGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    changed INTEGER;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM cloud_workers
        WHERE worker_id = p_worker_id
          AND instance_nonce = p_instance_nonce
          AND generation = p_generation
    ) THEN
        RETURN 0;
    END IF;

    WITH released AS (
        UPDATE cloud_session_commands
        SET state = CASE WHEN read_only THEN 'pending' ELSE 'indeterminate' END,
            lease_owner = NULL,
            worker_generation = NULL,
            dispatch_lease_until_ms = NULL,
            reply_json = CASE WHEN read_only THEN NULL ELSE jsonb_build_object(
                'command_id', command_id,
                'completed_at_ms', p_now_ms,
                'outcome', jsonb_build_object(
                    'status', 'error',
                    'error', jsonb_build_object(
                        'code', 'execution',
                        'message', 'cloud Worker released a mutation command with an unknown outcome'
                    )
                )
            ) END,
            completed_at_ms = CASE WHEN read_only THEN NULL ELSE p_now_ms END,
            updated_at_ms = p_now_ms
        WHERE state = 'inflight'
          AND lease_owner = p_worker_id
          AND worker_generation = p_generation
        RETURNING tenant_id, command_id, state
    ), cleared AS (
        UPDATE cloud_session_submissions AS submission
        SET steering_command_id = NULL,
            steering_target_run_id = NULL,
            steering_target_writer_fencing_token = NULL,
            placement = 'queued',
            updated_at_ms = p_now_ms
        FROM released
        WHERE submission.tenant_id = released.tenant_id
          AND submission.steering_command_id = released.command_id
          AND released.state = 'indeterminate'
        RETURNING 1
    )
    SELECT COUNT(*)::INTEGER INTO changed FROM released;
    RETURN changed;
END;
$$;

CREATE OR REPLACE FUNCTION ternilo_reap_cloud_session_commands(p_now_ms BIGINT) RETURNS INTEGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    changed INTEGER;
BEGIN
    WITH reaped AS (
        UPDATE cloud_session_commands
        SET state = CASE
                WHEN expires_at_ms <= p_now_ms THEN 'expired'
                WHEN read_only THEN 'pending'
                ELSE 'indeterminate'
            END,
            lease_owner = NULL,
            worker_generation = NULL,
            dispatch_lease_until_ms = NULL,
            reply_json = CASE
                WHEN expires_at_ms > p_now_ms AND read_only THEN NULL
                ELSE jsonb_build_object(
                    'command_id', command_id,
                    'completed_at_ms', p_now_ms,
                    'outcome', jsonb_build_object(
                        'status', 'error',
                        'error', jsonb_build_object(
                            'code', 'execution',
                            'message', CASE
                                WHEN expires_at_ms <= p_now_ms
                                THEN 'cloud Session command expired before completion'
                                ELSE 'cloud Session mutation command lost its dispatch owner'
                            END
                        )
                    )
                )
            END,
            completed_at_ms = CASE
                WHEN expires_at_ms > p_now_ms AND read_only THEN NULL
                ELSE p_now_ms
            END,
            updated_at_ms = p_now_ms
        WHERE (state = 'pending' AND expires_at_ms <= p_now_ms)
           OR (state = 'inflight' AND (
                expires_at_ms <= p_now_ms OR dispatch_lease_until_ms <= p_now_ms
           ))
        RETURNING tenant_id, command_id, state
    ), cleared AS (
        UPDATE cloud_session_submissions AS submission
        SET steering_command_id = NULL,
            steering_target_run_id = NULL,
            steering_target_writer_fencing_token = NULL,
            placement = 'queued',
            updated_at_ms = p_now_ms
        FROM reaped
        WHERE submission.tenant_id = reaped.tenant_id
          AND submission.steering_command_id = reaped.command_id
          AND reaped.state IN ('expired', 'indeterminate')
        RETURNING 1
    )
    SELECT COUNT(*)::INTEGER INTO changed FROM reaped;
    RETURN changed;
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
        PERFORM ternilo_requeue_cloud_steering_for_run(job.tenant_id, job.run_id, p_now_ms);
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

CREATE FUNCTION ternilo_defer_cloud_session_command(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_tenant_id TEXT,
    p_command_id TEXT,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    WITH deferred AS (
        UPDATE cloud_session_commands AS command
        SET state = 'pending',
            lease_owner = NULL,
            worker_generation = NULL,
            dispatch_lease_until_ms = NULL,
            updated_at_ms = p_now_ms
        WHERE command.tenant_id = p_tenant_id
          AND command.command_id = p_command_id
          AND command.state = 'inflight'
          AND command.lease_owner = p_worker_id
          AND command.worker_generation = p_generation
          AND command.expires_at_ms > p_now_ms
          AND EXISTS (
              SELECT 1
              FROM cloud_workers AS worker
              JOIN cloud_runs AS run
                ON run.tenant_id = command.tenant_id
               AND run.session_id = command.session_id
               AND run.run_id = command.target_run_id
              JOIN cloud_session_writer_leases AS writer
                ON writer.tenant_id = run.tenant_id
               AND writer.session_id = run.session_id
               AND writer.run_id = run.run_id
              WHERE worker.worker_id = p_worker_id
                AND worker.instance_nonce = p_instance_nonce
                AND worker.generation = p_generation
                AND worker.lease_expires_at_ms > p_now_ms
                AND run.state IN ('running', 'cancel_requested')
                AND run.lease_owner = p_worker_id
                AND run.session_fencing_token = command.target_writer_fencing_token
                AND writer.lease_owner = p_worker_id
                AND writer.fencing_token = command.target_writer_fencing_token
                AND writer.expires_at_ms > p_now_ms
          )
        RETURNING 1
    )
    SELECT EXISTS(SELECT 1 FROM deferred)
$$;

REVOKE ALL ON FUNCTION ternilo_cloud_steering_submission_for_worker(
    TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_run_submission_for_worker(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_complete_cloud_steering_command(
    TEXT, TEXT, BIGINT, TEXT, TEXT, BOOLEAN, JSONB, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_requeue_cloud_steering_for_run(TEXT, TEXT, BIGINT)
    FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_requeue_cloud_steering_for_run(
    TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT, BIGINT, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_consume_cloud_steering_event(
    TEXT, TEXT, TEXT, BIGINT, JSONB, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_defer_cloud_session_command(
    TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT
) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_cloud_run_submission_for_worker(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_steering_submission_for_worker(
            TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_complete_cloud_steering_command(
            TEXT, TEXT, BIGINT, TEXT, TEXT, BOOLEAN, JSONB, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_requeue_cloud_steering_for_run(
            TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT, BIGINT, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_defer_cloud_session_command(
            TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT
        ) TO ternilo_worker;
    END IF;
END;
$$;
