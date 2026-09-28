CREATE TABLE cloud_session_telemetry (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    sharing_status TEXT NOT NULL CHECK (sharing_status IN (
        'disabled', 'feedback_only', 'full'
    )),
    handoff_seq BIGINT NOT NULL DEFAULT -1 CHECK (handoff_seq >= -1),
    export_seq BIGINT NOT NULL DEFAULT -1 CHECK (export_seq >= -1),
    last_error TEXT,
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (tenant_id, user_id, session_id),
    FOREIGN KEY (tenant_id, session_id, user_id)
        REFERENCES cloud_sessions(tenant_id, session_id, user_id) ON DELETE CASCADE
);

CREATE TABLE cloud_telemetry_outbox (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    occurrence_id TEXT NOT NULL,
    from_seq BIGINT NOT NULL CHECK (from_seq >= 0),
    to_seq BIGINT NOT NULL CHECK (to_seq >= from_seq),
    state TEXT NOT NULL CHECK (state IN ('pending', 'inflight', 'exported')),
    lease_owner TEXT,
    worker_generation BIGINT,
    export_lease_until_ms BIGINT,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_error TEXT,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    exported_at_ms BIGINT,
    PRIMARY KEY (tenant_id, occurrence_id),
    UNIQUE (tenant_id, user_id, session_id, from_seq, to_seq),
    FOREIGN KEY (tenant_id, user_id, session_id)
        REFERENCES cloud_session_telemetry(tenant_id, user_id, session_id) ON DELETE CASCADE,
    CHECK (
        (state = 'pending' AND lease_owner IS NULL AND worker_generation IS NULL
            AND export_lease_until_ms IS NULL AND exported_at_ms IS NULL)
        OR (state = 'inflight' AND lease_owner IS NOT NULL AND worker_generation > 0
            AND export_lease_until_ms IS NOT NULL AND exported_at_ms IS NULL)
        OR (state = 'exported' AND lease_owner IS NULL AND worker_generation IS NULL
            AND export_lease_until_ms IS NULL AND exported_at_ms IS NOT NULL)
    )
);

CREATE INDEX cloud_telemetry_outbox_dispatch
ON cloud_telemetry_outbox (state, created_at_ms, occurrence_id)
WHERE state IN ('pending', 'inflight');

ALTER TABLE cloud_session_telemetry ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_telemetry_outbox ENABLE ROW LEVEL SECURITY;

CREATE POLICY cloud_session_telemetry_owner_scope ON cloud_session_telemetry
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE POLICY cloud_telemetry_outbox_owner_scope ON cloud_telemetry_outbox
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE FUNCTION ternilo_cloud_telemetry_sharing(p_plugins JSONB) RETURNS TEXT
LANGUAGE sql
IMMUTABLE
SET search_path = public, pg_temp
AS $$
    WITH plugin_rows AS (
        SELECT plugin, position,
               MIN(position) OVER (PARTITION BY plugin->>'id') AS first_position,
               ROW_NUMBER() OVER (
                   PARTITION BY plugin->>'id' ORDER BY position DESC
               ) AS precedence
        FROM jsonb_array_elements(COALESCE(p_plugins, '[]'::JSONB))
             WITH ORDINALITY AS item(plugin, position)
    ), effective_plugins AS (
        SELECT plugin, first_position
        FROM plugin_rows
        WHERE precedence = 1
    )
    SELECT COALESCE((
        SELECT CASE plugin->'config'->>'mode'
            WHEN 'feedback_only' THEN 'feedback_only'
            WHEN 'full' THEN 'full'
            ELSE 'disabled'
        END
        FROM effective_plugins
        WHERE plugin->>'kind' = 'ternilo.telemetry.otlp'
          AND COALESCE((plugin->>'enabled')::BOOLEAN, TRUE)
        ORDER BY first_position
        LIMIT 1
    ), 'disabled')
$$;

CREATE FUNCTION ternilo_enqueue_cloud_telemetry(
    p_tenant_id TEXT,
    p_user_id TEXT,
    p_session_id TEXT,
    p_from_seq BIGINT,
    p_to_seq BIGINT,
    p_now_ms BIGINT
) RETURNS VOID
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    occurrence TEXT;
BEGIN
    IF p_from_seq > p_to_seq THEN
        RETURN;
    END IF;
    occurrence := 'tel_' || md5(
        p_tenant_id || E'\n' || p_user_id || E'\n' || p_session_id || E'\n'
        || p_from_seq::TEXT || E'\n' || p_to_seq::TEXT
    );
    INSERT INTO cloud_telemetry_outbox (
        tenant_id, user_id, session_id, occurrence_id, from_seq, to_seq,
        state, created_at_ms, updated_at_ms
    ) VALUES (
        p_tenant_id, p_user_id, p_session_id, occurrence,
        p_from_seq, p_to_seq, 'pending', p_now_ms, p_now_ms
    )
    ON CONFLICT (tenant_id, user_id, session_id, from_seq, to_seq) DO NOTHING;
END;
$$;

CREATE FUNCTION ternilo_sync_cloud_session_telemetry() RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    next_status TEXT;
    prior cloud_session_telemetry%ROWTYPE;
    initial_handoff BIGINT;
BEGIN
    next_status := ternilo_cloud_telemetry_sharing(NEW.profile_plugins);
    SELECT * INTO prior
    FROM cloud_session_telemetry
    WHERE tenant_id = NEW.tenant_id AND user_id = NEW.user_id
      AND session_id = NEW.session_id
    FOR UPDATE;

    IF NOT FOUND THEN
        initial_handoff := CASE WHEN next_status = 'disabled' THEN NEW.last_seq ELSE -1 END;
        INSERT INTO cloud_session_telemetry (
            tenant_id, user_id, session_id, sharing_status,
            handoff_seq, export_seq, updated_at_ms
        ) VALUES (
            NEW.tenant_id, NEW.user_id, NEW.session_id, next_status,
            initial_handoff, initial_handoff, NEW.updated_at_ms
        );
        IF next_status = 'full' AND NEW.last_seq >= 0 THEN
            PERFORM ternilo_enqueue_cloud_telemetry(
                NEW.tenant_id, NEW.user_id, NEW.session_id,
                0, NEW.last_seq, NEW.updated_at_ms
            );
            UPDATE cloud_session_telemetry SET handoff_seq = NEW.last_seq
            WHERE tenant_id = NEW.tenant_id AND user_id = NEW.user_id
              AND session_id = NEW.session_id;
        END IF;
        RETURN NEW;
    END IF;

    IF prior.sharing_status = next_status THEN
        IF next_status = 'disabled' THEN
            DELETE FROM cloud_telemetry_outbox
            WHERE tenant_id = NEW.tenant_id AND user_id = NEW.user_id
              AND session_id = NEW.session_id AND state = 'pending';
        END IF;
        RETURN NEW;
    END IF;
    IF next_status = 'full' AND NEW.last_seq > prior.handoff_seq THEN
        PERFORM ternilo_enqueue_cloud_telemetry(
            NEW.tenant_id, NEW.user_id, NEW.session_id,
            prior.handoff_seq + 1, NEW.last_seq, NEW.updated_at_ms
        );
        prior.handoff_seq := NEW.last_seq;
    ELSIF prior.sharing_status = 'disabled' OR next_status = 'disabled' THEN
        prior.handoff_seq := NEW.last_seq;
    END IF;
    UPDATE cloud_session_telemetry
    SET sharing_status = next_status,
        handoff_seq = prior.handoff_seq,
        export_seq = CASE
            WHEN next_status = 'disabled' THEN GREATEST(export_seq, NEW.last_seq)
            ELSE export_seq
        END,
        last_error = NULL,
        updated_at_ms = GREATEST(updated_at_ms, NEW.updated_at_ms)
    WHERE tenant_id = NEW.tenant_id AND user_id = NEW.user_id
      AND session_id = NEW.session_id;
    IF next_status = 'disabled' THEN
        DELETE FROM cloud_telemetry_outbox
        WHERE tenant_id = NEW.tenant_id AND user_id = NEW.user_id
          AND session_id = NEW.session_id AND state = 'pending';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER cloud_session_telemetry_sync
AFTER INSERT OR UPDATE OF profile_plugins ON cloud_sessions
FOR EACH ROW EXECUTE FUNCTION ternilo_sync_cloud_session_telemetry();

CREATE FUNCTION ternilo_capture_cloud_session_telemetry() RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    telemetry cloud_session_telemetry%ROWTYPE;
    release BOOLEAN;
BEGIN
    SELECT * INTO telemetry
    FROM cloud_session_telemetry
    WHERE tenant_id = NEW.tenant_id AND session_id = NEW.session_id
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN NEW;
    END IF;
    release := telemetry.sharing_status = 'full'
        OR (telemetry.sharing_status = 'feedback_only'
            AND NEW.event->>'type' IN ('feedback_recorded', 'feedback_submitted'));
    IF release AND NEW.seq > telemetry.handoff_seq THEN
        PERFORM ternilo_enqueue_cloud_telemetry(
            NEW.tenant_id, telemetry.user_id, NEW.session_id,
            telemetry.handoff_seq + 1, NEW.seq, NEW.created_at_ms
        );
        UPDATE cloud_session_telemetry
        SET handoff_seq = NEW.seq,
            updated_at_ms = GREATEST(updated_at_ms, NEW.created_at_ms)
        WHERE tenant_id = NEW.tenant_id AND user_id = telemetry.user_id
          AND session_id = NEW.session_id;
    ELSIF telemetry.sharing_status = 'disabled' AND NEW.seq > telemetry.handoff_seq THEN
        UPDATE cloud_session_telemetry
        SET handoff_seq = NEW.seq, export_seq = GREATEST(export_seq, NEW.seq),
            updated_at_ms = GREATEST(updated_at_ms, NEW.created_at_ms)
        WHERE tenant_id = NEW.tenant_id AND user_id = telemetry.user_id
          AND session_id = NEW.session_id;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER cloud_session_telemetry_capture
AFTER INSERT ON cloud_session_events
FOR EACH ROW EXECUTE FUNCTION ternilo_capture_cloud_session_telemetry();

CREATE FUNCTION ternilo_claim_cloud_telemetry(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_now_ms BIGINT,
    p_export_lease_until_ms BIGINT,
    p_limit INTEGER
) RETURNS TABLE (
    tenant_id TEXT,
    user_id TEXT,
    session_id TEXT,
    agent_id TEXT,
    occurrence_id TEXT,
    from_seq BIGINT,
    to_seq BIGINT,
    attempt_count INTEGER,
    events JSONB
)
LANGUAGE sql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    WITH current_worker AS MATERIALIZED (
        SELECT worker_id
        FROM cloud_workers
        WHERE worker_id = p_worker_id
          AND instance_nonce = p_instance_nonce
          AND generation = p_generation
          AND lease_expires_at_ms > p_now_ms
          AND p_export_lease_until_ms > p_now_ms
          AND hello_json->'capabilities' ? 'telemetry_disclosure'
    ), candidates AS (
        SELECT pending.tenant_id, pending.occurrence_id
        FROM cloud_telemetry_outbox AS pending
        JOIN cloud_session_telemetry AS telemetry
          ON telemetry.tenant_id = pending.tenant_id
         AND telemetry.user_id = pending.user_id
         AND telemetry.session_id = pending.session_id
         AND telemetry.sharing_status <> 'disabled'
        CROSS JOIN current_worker
        WHERE (
            pending.state = 'pending'
            OR (pending.state = 'inflight' AND pending.export_lease_until_ms <= p_now_ms)
          )
          AND pending.to_seq > telemetry.export_seq
          AND NOT EXISTS (
              SELECT 1 FROM cloud_telemetry_outbox AS earlier
              WHERE earlier.tenant_id = pending.tenant_id
                AND earlier.user_id = pending.user_id
                AND earlier.session_id = pending.session_id
                AND earlier.to_seq < pending.from_seq
                AND earlier.to_seq > telemetry.export_seq
                AND earlier.state <> 'exported'
          )
        ORDER BY pending.created_at_ms, pending.occurrence_id
        FOR UPDATE OF pending SKIP LOCKED
        LIMIT p_limit
    ), claimed AS (
        UPDATE cloud_telemetry_outbox AS item
        SET state = 'inflight', lease_owner = p_worker_id,
            worker_generation = p_generation,
            export_lease_until_ms = p_export_lease_until_ms,
            attempt_count = item.attempt_count + 1,
            last_error = NULL,
            updated_at_ms = GREATEST(item.updated_at_ms, p_now_ms)
        FROM candidates
        WHERE item.tenant_id = candidates.tenant_id
          AND item.occurrence_id = candidates.occurrence_id
        RETURNING item.*
    )
    SELECT claimed.tenant_id, claimed.user_id, claimed.session_id,
           session.agent_id, claimed.occurrence_id, claimed.from_seq,
           claimed.to_seq, claimed.attempt_count,
           COALESCE(jsonb_agg(event.event ORDER BY event.seq)
               FILTER (WHERE event.event IS NOT NULL), '[]'::JSONB) AS events
    FROM claimed
    JOIN cloud_sessions AS session
      ON session.tenant_id = claimed.tenant_id
     AND session.user_id = claimed.user_id
     AND session.session_id = claimed.session_id
    LEFT JOIN cloud_session_events AS event
      ON event.tenant_id = claimed.tenant_id
     AND event.session_id = claimed.session_id
     AND event.seq BETWEEN claimed.from_seq AND claimed.to_seq
     AND NOT (
        event.event->>'type' IN ('assistant_message_delta', 'assistant_reasoning_delta')
        AND EXISTS (
            SELECT 1 FROM cloud_session_events AS first_chunk
            WHERE first_chunk.tenant_id = event.tenant_id
              AND first_chunk.session_id = event.session_id
              AND first_chunk.run_id = event.run_id
              AND first_chunk.seq < event.seq
              AND first_chunk.event->>'type' IN (
                  'assistant_message_delta', 'assistant_reasoning_delta'
              )
              AND first_chunk.event->>'step' = event.event->>'step'
        )
     )
    GROUP BY claimed.tenant_id, claimed.user_id, claimed.session_id,
             session.agent_id, claimed.occurrence_id, claimed.from_seq,
             claimed.to_seq, claimed.attempt_count
$$;

CREATE FUNCTION ternilo_ack_cloud_telemetry(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_tenant_id TEXT,
    p_occurrence_id TEXT,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    acknowledged cloud_telemetry_outbox%ROWTYPE;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM cloud_workers
        WHERE worker_id = p_worker_id AND instance_nonce = p_instance_nonce
          AND generation = p_generation AND lease_expires_at_ms > p_now_ms
    ) THEN
        RETURN FALSE;
    END IF;
    UPDATE cloud_telemetry_outbox
    SET state = 'exported', lease_owner = NULL, worker_generation = NULL,
        export_lease_until_ms = NULL,
        exported_at_ms = GREATEST(updated_at_ms, p_now_ms),
        updated_at_ms = GREATEST(updated_at_ms, p_now_ms), last_error = NULL
    WHERE tenant_id = p_tenant_id AND occurrence_id = p_occurrence_id
      AND state = 'inflight' AND lease_owner = p_worker_id
      AND worker_generation = p_generation AND export_lease_until_ms > p_now_ms
    RETURNING * INTO acknowledged;
    IF NOT FOUND THEN
        RETURN EXISTS (
            SELECT 1 FROM cloud_telemetry_outbox
            WHERE tenant_id = p_tenant_id AND occurrence_id = p_occurrence_id
              AND state = 'exported'
        );
    END IF;
    UPDATE cloud_session_telemetry
    SET export_seq = GREATEST(export_seq, acknowledged.to_seq),
        last_error = NULL,
        updated_at_ms = GREATEST(updated_at_ms, p_now_ms)
    WHERE tenant_id = acknowledged.tenant_id
      AND user_id = acknowledged.user_id
      AND session_id = acknowledged.session_id;
    RETURN TRUE;
END;
$$;

CREATE FUNCTION ternilo_fail_cloud_telemetry(
    p_worker_id TEXT,
    p_instance_nonce TEXT,
    p_generation BIGINT,
    p_tenant_id TEXT,
    p_occurrence_id TEXT,
    p_error TEXT,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    failed_tenant_id TEXT;
    failed_user_id TEXT;
    failed_session_id TEXT;
    failed_to_seq BIGINT;
    sharing_status_value TEXT;
    export_seq_value BIGINT;
BEGIN
    SELECT telemetry.sharing_status, telemetry.export_seq
    INTO sharing_status_value, export_seq_value
    FROM cloud_session_telemetry AS telemetry
    JOIN cloud_telemetry_outbox AS item
      ON item.tenant_id = telemetry.tenant_id
     AND item.user_id = telemetry.user_id
     AND item.session_id = telemetry.session_id
    WHERE item.tenant_id = p_tenant_id AND item.occurrence_id = p_occurrence_id
      AND item.state = 'inflight' AND item.lease_owner = p_worker_id
      AND item.worker_generation = p_generation
      AND EXISTS (
          SELECT 1 FROM cloud_workers
          WHERE worker_id = p_worker_id AND instance_nonce = p_instance_nonce
            AND generation = p_generation
      )
    FOR UPDATE OF telemetry;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    SELECT tenant_id, user_id, session_id, to_seq
    INTO failed_tenant_id, failed_user_id, failed_session_id, failed_to_seq
    FROM cloud_telemetry_outbox
    WHERE tenant_id = p_tenant_id AND occurrence_id = p_occurrence_id
      AND state = 'inflight' AND lease_owner = p_worker_id
      AND worker_generation = p_generation
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    IF sharing_status_value = 'disabled' OR failed_to_seq <= export_seq_value THEN
        DELETE FROM cloud_telemetry_outbox
        WHERE tenant_id = p_tenant_id AND occurrence_id = p_occurrence_id;
        RETURN TRUE;
    END IF;
    UPDATE cloud_telemetry_outbox
    SET state = 'pending', lease_owner = NULL, worker_generation = NULL,
        export_lease_until_ms = NULL, last_error = p_error,
        updated_at_ms = GREATEST(updated_at_ms, p_now_ms)
    WHERE tenant_id = p_tenant_id AND occurrence_id = p_occurrence_id
      AND state = 'inflight' AND lease_owner = p_worker_id
      AND worker_generation = p_generation
    RETURNING tenant_id, user_id, session_id
    INTO failed_tenant_id, failed_user_id, failed_session_id;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    UPDATE cloud_session_telemetry
    SET last_error = p_error,
        updated_at_ms = GREATEST(updated_at_ms, p_now_ms)
    WHERE tenant_id = failed_tenant_id AND user_id = failed_user_id
      AND session_id = failed_session_id;
    RETURN TRUE;
END;
$$;

CREATE FUNCTION ternilo_release_cloud_telemetry(
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
        WHERE worker_id = p_worker_id AND instance_nonce = p_instance_nonce
          AND generation = p_generation
    ) THEN
        RETURN 0;
    END IF;
    WITH discarded AS (
        DELETE FROM cloud_telemetry_outbox AS item
        USING cloud_session_telemetry AS telemetry
        WHERE item.state = 'inflight' AND item.lease_owner = p_worker_id
          AND item.worker_generation = p_generation
          AND telemetry.tenant_id = item.tenant_id
          AND telemetry.user_id = item.user_id
          AND telemetry.session_id = item.session_id
          AND (
              telemetry.sharing_status = 'disabled'
              OR item.to_seq <= telemetry.export_seq
          )
        RETURNING 1
    ), released AS (
        UPDATE cloud_telemetry_outbox AS item
        SET state = 'pending', lease_owner = NULL, worker_generation = NULL,
            export_lease_until_ms = NULL,
            updated_at_ms = GREATEST(item.updated_at_ms, p_now_ms)
        WHERE item.state = 'inflight' AND item.lease_owner = p_worker_id
          AND item.worker_generation = p_generation
          AND EXISTS (
              SELECT 1 FROM cloud_session_telemetry AS telemetry
              WHERE telemetry.tenant_id = item.tenant_id
                AND telemetry.user_id = item.user_id
                AND telemetry.session_id = item.session_id
                AND telemetry.sharing_status <> 'disabled'
                AND item.to_seq > telemetry.export_seq
          )
        RETURNING 1
    )
    SELECT (
        (SELECT COUNT(*) FROM discarded) + (SELECT COUNT(*) FROM released)
    )::INTEGER INTO changed;
    RETURN changed;
END;
$$;

CREATE OR REPLACE FUNCTION ternilo_drain_cloud_worker(
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
    released_telemetry INTEGER;
    reaped_runs INTEGER;
BEGIN
    PERFORM 1 FROM cloud_workers
    WHERE worker_id = p_worker_id AND instance_nonce = p_instance_nonce
      AND generation = p_generation
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN 0;
    END IF;
    released_commands := ternilo_release_cloud_session_commands(
        p_worker_id, p_instance_nonce, p_generation, p_now_ms
    );
    released_telemetry := ternilo_release_cloud_telemetry(
        p_worker_id, p_instance_nonce, p_generation, p_now_ms
    );
    UPDATE cloud_runs SET lease_expires_at_ms = p_now_ms, updated_at_ms = p_now_ms
    WHERE lease_owner = p_worker_id
      AND state IN ('leased', 'running', 'cancel_requested');
    UPDATE cloud_session_writer_leases SET expires_at_ms = p_now_ms
    WHERE lease_owner = p_worker_id;
    reaped_runs := ternilo_reap_cloud_runs(p_now_ms);
    RETURN released_commands + released_telemetry + reaped_runs;
END;
$$;

INSERT INTO cloud_session_telemetry (
    tenant_id, user_id, session_id, sharing_status,
    handoff_seq, export_seq, updated_at_ms
)
SELECT tenant_id, user_id, session_id,
       ternilo_cloud_telemetry_sharing(profile_plugins),
       CASE WHEN ternilo_cloud_telemetry_sharing(profile_plugins) = 'disabled'
            THEN last_seq ELSE -1 END,
       CASE WHEN ternilo_cloud_telemetry_sharing(profile_plugins) = 'disabled'
            THEN last_seq ELSE -1 END,
       updated_at_ms
FROM cloud_sessions
ON CONFLICT DO NOTHING;

DO $$
DECLARE
    session RECORD;
BEGIN
    FOR session IN
        SELECT telemetry.tenant_id, telemetry.user_id, telemetry.session_id,
               cloud.last_seq, cloud.updated_at_ms
        FROM cloud_session_telemetry AS telemetry
        JOIN cloud_sessions AS cloud
          ON cloud.tenant_id = telemetry.tenant_id
         AND cloud.user_id = telemetry.user_id
         AND cloud.session_id = telemetry.session_id
        WHERE telemetry.sharing_status = 'full' AND cloud.last_seq >= 0
    LOOP
        PERFORM ternilo_enqueue_cloud_telemetry(
            session.tenant_id, session.user_id, session.session_id,
            0, session.last_seq, session.updated_at_ms
        );
        UPDATE cloud_session_telemetry SET handoff_seq = session.last_seq
        WHERE tenant_id = session.tenant_id AND user_id = session.user_id
          AND session_id = session.session_id;
    END LOOP;
END;
$$;

REVOKE ALL ON FUNCTION ternilo_cloud_telemetry_sharing(JSONB) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_enqueue_cloud_telemetry(TEXT, TEXT, TEXT, BIGINT, BIGINT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_sync_cloud_session_telemetry() FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_capture_cloud_session_telemetry() FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_claim_cloud_telemetry(TEXT, TEXT, BIGINT, BIGINT, BIGINT, INTEGER) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_ack_cloud_telemetry(TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_fail_cloud_telemetry(TEXT, TEXT, BIGINT, TEXT, TEXT, TEXT, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_release_cloud_telemetry(TEXT, TEXT, BIGINT, BIGINT) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_claim_cloud_telemetry(TEXT, TEXT, BIGINT, BIGINT, BIGINT, INTEGER)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_ack_cloud_telemetry(TEXT, TEXT, BIGINT, TEXT, TEXT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_fail_cloud_telemetry(TEXT, TEXT, BIGINT, TEXT, TEXT, TEXT, BIGINT)
            TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_release_cloud_telemetry(TEXT, TEXT, BIGINT, BIGINT)
            TO ternilo_worker;
    END IF;
END;
$$;
