CREATE TABLE cloud_workers (
    worker_id TEXT PRIMARY KEY,
    instance_nonce TEXT NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    hello_json JSONB NOT NULL,
    registered_at_ms BIGINT NOT NULL CHECK (registered_at_ms >= 0),
    last_seen_at_ms BIGINT NOT NULL CHECK (last_seen_at_ms >= registered_at_ms),
    lease_expires_at_ms BIGINT NOT NULL CHECK (lease_expires_at_ms > last_seen_at_ms)
);

CREATE TABLE cloud_session_commands (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    command_seq BIGINT NOT NULL CHECK (command_seq >= 0),
    command_json JSONB NOT NULL,
    command_digest BYTEA NOT NULL CHECK (octet_length(command_digest) = 32),
    required_capability TEXT NOT NULL,
    required_catalog_revision TEXT,
    read_only BOOLEAN NOT NULL,
    target_run_id TEXT,
    target_writer_fencing_token BIGINT,
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'inflight', 'completed', 'expired', 'indeterminate'
    )),
    lease_owner TEXT,
    worker_generation BIGINT,
    dispatch_lease_until_ms BIGINT,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    reply_json JSONB,
    issued_at_ms BIGINT NOT NULL CHECK (issued_at_ms >= 0),
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms > issued_at_ms),
    completed_at_ms BIGINT,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, command_id),
    UNIQUE (tenant_id, user_id, session_id, command_seq),
    FOREIGN KEY (tenant_id, session_id, user_id)
        REFERENCES cloud_sessions(tenant_id, session_id, user_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, target_run_id)
        REFERENCES cloud_runs(tenant_id, run_id),
    CHECK (
        (target_run_id IS NULL AND target_writer_fencing_token IS NULL)
        OR (target_run_id IS NOT NULL AND target_writer_fencing_token > 0)
    ),
    CHECK (read_only OR target_run_id IS NOT NULL),
    CHECK (
        (state = 'pending'
            AND lease_owner IS NULL
            AND worker_generation IS NULL
            AND dispatch_lease_until_ms IS NULL
            AND reply_json IS NULL
            AND completed_at_ms IS NULL)
        OR (state = 'inflight'
            AND lease_owner IS NOT NULL
            AND worker_generation > 0
            AND dispatch_lease_until_ms IS NOT NULL
            AND reply_json IS NULL
            AND completed_at_ms IS NULL)
        OR (state IN ('completed', 'expired', 'indeterminate')
            AND lease_owner IS NULL
            AND worker_generation IS NULL
            AND dispatch_lease_until_ms IS NULL
            AND reply_json IS NOT NULL
            AND completed_at_ms IS NOT NULL)
    )
);

CREATE INDEX cloud_session_commands_dispatch
ON cloud_session_commands (state, expires_at_ms, command_seq)
WHERE state IN ('pending', 'inflight');

CREATE INDEX cloud_session_commands_session
ON cloud_session_commands (tenant_id, user_id, session_id, command_seq);

ALTER TABLE cloud_session_commands ENABLE ROW LEVEL SECURITY;

CREATE POLICY cloud_session_commands_owner_scope ON cloud_session_commands
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE FUNCTION ternilo_register_cloud_worker(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_hello_json JSONB,
    p_now_ms BIGINT,
    p_lease_expires_at_ms BIGINT
) RETURNS BIGINT
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    registered_generation BIGINT;
BEGIN
    IF p_lease_expires_at_ms <= p_now_ms
       OR p_hello_json->>'executor_id' IS DISTINCT FROM p_worker_id
       OR p_hello_json->>'instance_nonce' IS DISTINCT FROM p_instance_nonce
       OR p_hello_json->>'executor_kind' IS DISTINCT FROM 'cloud_worker' THEN
        RETURN NULL;
    END IF;

    INSERT INTO cloud_workers (
        worker_id, instance_nonce, generation, hello_json,
        registered_at_ms, last_seen_at_ms, lease_expires_at_ms
    ) VALUES (
        p_worker_id, p_instance_nonce, 1, p_hello_json,
        p_now_ms, p_now_ms, p_lease_expires_at_ms
    )
    ON CONFLICT (worker_id) DO UPDATE
    SET instance_nonce = EXCLUDED.instance_nonce,
        generation = CASE
            WHEN cloud_workers.instance_nonce = EXCLUDED.instance_nonce
            THEN cloud_workers.generation
            ELSE cloud_workers.generation + 1
        END,
        hello_json = EXCLUDED.hello_json,
        registered_at_ms = CASE
            WHEN cloud_workers.instance_nonce = EXCLUDED.instance_nonce
            THEN cloud_workers.registered_at_ms
            ELSE EXCLUDED.registered_at_ms
        END,
        last_seen_at_ms = EXCLUDED.last_seen_at_ms,
        lease_expires_at_ms = EXCLUDED.lease_expires_at_ms
    RETURNING generation INTO registered_generation;

    RETURN registered_generation;
END;
$$;

CREATE FUNCTION ternilo_heartbeat_cloud_worker(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_now_ms BIGINT,
    p_lease_expires_at_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    WITH heartbeat AS (
        UPDATE cloud_workers
        SET last_seen_at_ms = p_now_ms,
            lease_expires_at_ms = p_lease_expires_at_ms
        WHERE worker_id = p_worker_id
          AND instance_nonce = p_instance_nonce
          AND generation = p_generation
          AND lease_expires_at_ms > p_now_ms
          AND p_lease_expires_at_ms > p_now_ms
        RETURNING 1
    )
    SELECT EXISTS(SELECT 1 FROM heartbeat)
$$;

CREATE FUNCTION ternilo_claim_cloud_session_commands(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_capabilities TEXT[],
    p_now_ms BIGINT,
    p_dispatch_lease_until_ms BIGINT,
    p_limit INTEGER
) RETURNS TABLE (
    tenant_id TEXT,
    user_id TEXT,
    session_id TEXT,
    command_id TEXT,
    command_seq BIGINT,
    command_json JSONB,
    required_capability TEXT,
    read_only BOOLEAN,
    target_run_id TEXT,
    target_writer_fencing_token BIGINT,
    attempt_count INTEGER
)
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    WITH current_worker AS MATERIALIZED (
        SELECT worker_id, hello_json
        FROM cloud_workers
        WHERE worker_id = p_worker_id
          AND instance_nonce = p_instance_nonce
          AND generation = p_generation
          AND lease_expires_at_ms > p_now_ms
          AND p_dispatch_lease_until_ms > p_now_ms
          AND 'addressed_session_commands' = ANY(p_capabilities)
    ), candidates AS (
        SELECT pending.tenant_id, pending.command_id
        FROM cloud_session_commands AS pending
        CROSS JOIN current_worker
        WHERE pending.expires_at_ms > p_now_ms
          AND pending.required_capability = ANY(p_capabilities)
          AND (
              pending.required_catalog_revision IS NULL
              OR pending.required_catalog_revision = current_worker.hello_json->>'catalog_revision'
          )
          AND (
              pending.state = 'pending'
              OR (
                  pending.state = 'inflight'
                  AND pending.read_only
                  AND pending.dispatch_lease_until_ms <= p_now_ms
              )
          )
          AND (
              (
                  pending.target_run_id IS NOT NULL
                  AND EXISTS (
                  SELECT 1
                  FROM cloud_runs AS run
                  JOIN cloud_session_writer_leases AS writer
                    ON writer.tenant_id = run.tenant_id
                   AND writer.session_id = run.session_id
                  WHERE run.tenant_id = pending.tenant_id
                    AND run.session_id = pending.session_id
                    AND run.run_id = pending.target_run_id
                    AND run.state IN ('running', 'cancel_requested')
                    AND run.lease_owner = p_worker_id
                    AND run.session_fencing_token = pending.target_writer_fencing_token
                    AND writer.run_id = run.run_id
                    AND writer.lease_owner = p_worker_id
                    AND writer.fencing_token = pending.target_writer_fencing_token
                    AND writer.expires_at_ms > p_now_ms
                  )
              )
              OR (
                  pending.target_run_id IS NULL
                  AND (
                      NOT EXISTS (
                          SELECT 1 FROM cloud_runs AS active
                          WHERE active.tenant_id = pending.tenant_id
                            AND active.session_id = pending.session_id
                            AND active.state IN ('running', 'cancel_requested')
                            AND active.lease_expires_at_ms > p_now_ms
                      )
                      OR EXISTS (
                          SELECT 1 FROM cloud_runs AS active
                          WHERE active.tenant_id = pending.tenant_id
                            AND active.session_id = pending.session_id
                            AND active.state IN ('running', 'cancel_requested')
                            AND active.lease_owner = p_worker_id
                            AND active.lease_expires_at_ms > p_now_ms
                      )
                  )
              )
          )
        ORDER BY pending.issued_at_ms, pending.command_seq
        FOR UPDATE OF pending SKIP LOCKED
        LIMIT p_limit
    )
    UPDATE cloud_session_commands AS claimed
    SET state = 'inflight',
        lease_owner = p_worker_id,
        worker_generation = p_generation,
        dispatch_lease_until_ms = p_dispatch_lease_until_ms,
        attempt_count = claimed.attempt_count + 1,
        updated_at_ms = GREATEST(claimed.updated_at_ms, p_now_ms)
    FROM candidates
    WHERE claimed.tenant_id = candidates.tenant_id
      AND claimed.command_id = candidates.command_id
    RETURNING claimed.tenant_id, claimed.user_id, claimed.session_id,
              claimed.command_id, claimed.command_seq, claimed.command_json,
              claimed.required_capability, claimed.read_only,
              claimed.target_run_id, claimed.target_writer_fencing_token,
              claimed.attempt_count
$$;

CREATE FUNCTION ternilo_complete_cloud_session_command(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_tenant_id TEXT,
    p_command_id TEXT,
    p_reply_json JSONB,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
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

    UPDATE cloud_session_commands AS command
    SET state = 'completed',
        lease_owner = NULL,
        worker_generation = NULL,
        dispatch_lease_until_ms = NULL,
        reply_json = p_reply_json,
        completed_at_ms = p_now_ms,
        updated_at_ms = p_now_ms
    WHERE command.tenant_id = p_tenant_id
      AND command.command_id = p_command_id
      AND command.state = 'inflight'
      AND command.lease_owner = p_worker_id
      AND command.worker_generation = p_generation
      AND command.dispatch_lease_until_ms > p_now_ms
      AND (
          command.target_run_id IS NULL
          OR EXISTS (
              SELECT 1
              FROM cloud_runs AS run
              JOIN cloud_session_writer_leases AS writer
                ON writer.tenant_id = run.tenant_id
               AND writer.session_id = run.session_id
              WHERE run.tenant_id = command.tenant_id
                AND run.session_id = command.session_id
                AND run.run_id = command.target_run_id
                AND run.state IN ('running', 'cancel_requested')
                AND run.lease_owner = p_worker_id
                AND run.session_fencing_token = command.target_writer_fencing_token
                AND writer.run_id = run.run_id
                AND writer.lease_owner = p_worker_id
                AND writer.fencing_token = command.target_writer_fencing_token
                AND writer.expires_at_ms > p_now_ms
          )
      );
    GET DIAGNOSTICS changed = ROW_COUNT;
    IF changed = 1 THEN
        RETURN TRUE;
    END IF;

    RETURN EXISTS (
        SELECT 1 FROM cloud_session_commands
        WHERE tenant_id = p_tenant_id
          AND command_id = p_command_id
          AND state = 'completed'
          AND reply_json = p_reply_json
    );
END;
$$;

CREATE FUNCTION ternilo_cloud_session_for_worker_command(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_tenant_id TEXT,
    p_command_id TEXT,
    p_now_ms BIGINT
) RETURNS SETOF cloud_sessions
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT session.*
    FROM cloud_workers AS worker
    JOIN cloud_session_commands AS command
      ON command.lease_owner = worker.worker_id
     AND command.worker_generation = worker.generation
    JOIN cloud_sessions AS session
      ON session.tenant_id = command.tenant_id
     AND session.user_id = command.user_id
     AND session.session_id = command.session_id
    WHERE worker.worker_id = p_worker_id
      AND worker.instance_nonce = p_instance_nonce
      AND worker.generation = p_generation
      AND worker.lease_expires_at_ms > p_now_ms
      AND command.tenant_id = p_tenant_id
      AND command.command_id = p_command_id
      AND command.state = 'inflight'
      AND command.read_only
      AND command.dispatch_lease_until_ms > p_now_ms
$$;

CREATE FUNCTION ternilo_release_cloud_session_commands(
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
        RETURNING 1
    )
    SELECT COUNT(*)::INTEGER INTO changed FROM released;
    RETURN changed;
END;
$$;

CREATE FUNCTION ternilo_reap_cloud_session_commands(p_now_ms BIGINT) RETURNS INTEGER
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
        RETURNING 1
    )
    SELECT COUNT(*)::INTEGER INTO changed FROM reaped;
    RETURN changed;
END;
$$;

CREATE FUNCTION ternilo_drain_cloud_worker(
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
    released_commands INTEGER;
    reaped_runs INTEGER;
BEGIN
    PERFORM 1
    FROM cloud_workers
    WHERE worker_id = p_worker_id
      AND instance_nonce = p_instance_nonce
      AND generation = p_generation
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN 0;
    END IF;

    released_commands := ternilo_release_cloud_session_commands(
        p_worker_id, p_instance_nonce, p_generation, p_now_ms
    );

    UPDATE cloud_runs
    SET lease_expires_at_ms = p_now_ms,
        updated_at_ms = p_now_ms
    WHERE lease_owner = p_worker_id
      AND state IN ('leased', 'running', 'cancel_requested');

    UPDATE cloud_session_writer_leases
    SET expires_at_ms = p_now_ms
    WHERE lease_owner = p_worker_id;

    reaped_runs := ternilo_reap_cloud_runs(p_now_ms);
    RETURN released_commands + reaped_runs;
END;
$$;

REVOKE ALL ON FUNCTION ternilo_register_cloud_worker(TEXT, TEXT, JSONB, BIGINT, BIGINT)
    FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_heartbeat_cloud_worker(TEXT, TEXT, BIGINT, BIGINT, BIGINT)
    FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_claim_cloud_session_commands(TEXT, TEXT, BIGINT, TEXT[], BIGINT, BIGINT, INTEGER)
    FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_complete_cloud_session_command(TEXT, TEXT, BIGINT, TEXT, TEXT, JSONB, BIGINT)
    FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_session_for_worker_command(TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT)
    FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_release_cloud_session_commands(TEXT, TEXT, BIGINT, BIGINT)
    FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_reap_cloud_session_commands(BIGINT)
    FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_drain_cloud_worker(TEXT, TEXT, BIGINT, BIGINT)
    FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_register_cloud_worker(TEXT, TEXT, JSONB, BIGINT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_heartbeat_cloud_worker(TEXT, TEXT, BIGINT, BIGINT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_claim_cloud_session_commands(TEXT, TEXT, BIGINT, TEXT[], BIGINT, BIGINT, INTEGER)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_complete_cloud_session_command(TEXT, TEXT, BIGINT, TEXT, TEXT, JSONB, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_session_for_worker_command(TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_release_cloud_session_commands(TEXT, TEXT, BIGINT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_reap_cloud_session_commands(BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_drain_cloud_worker(TEXT, TEXT, BIGINT, BIGINT)
            TO ternilo_worker;
    END IF;
END;
$$;
