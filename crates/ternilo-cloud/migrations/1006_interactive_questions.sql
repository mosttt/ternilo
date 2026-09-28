CREATE TABLE cloud_session_questions (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    question_id TEXT NOT NULL CHECK (length(question_id) BETWEEN 1 AND 256),
    question JSONB NOT NULL,
    answer JSONB,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'answered')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    answered_at_ms BIGINT,
    PRIMARY KEY (tenant_id, session_id, question_id),
    FOREIGN KEY (tenant_id, session_id, user_id)
        REFERENCES cloud_sessions(tenant_id, session_id, user_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, run_id)
        REFERENCES cloud_runs(tenant_id, run_id) ON DELETE CASCADE
);

CREATE INDEX cloud_session_questions_pending
ON cloud_session_questions (tenant_id, user_id, session_id, created_at_ms)
WHERE state = 'pending';

ALTER TABLE cloud_session_questions ENABLE ROW LEVEL SECURITY;

CREATE POLICY cloud_session_questions_owner_scope ON cloud_session_questions
USING (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
)
WITH CHECK (
    tenant_id = current_setting('ternilo.tenant_id', true)
    AND user_id = current_setting('ternilo.user_id', true)
);

CREATE FUNCTION ternilo_record_cloud_question(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_question_id TEXT,
    p_question JSONB,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    job cloud_runs%ROWTYPE;
    existing_question JSONB;
BEGIN
    SELECT run.* INTO job
    FROM cloud_runs AS run
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = run.tenant_id AND writer.session_id = run.session_id
    WHERE run.tenant_id = p_tenant_id AND run.run_id = p_run_id
      AND run.state IN ('running', 'cancel_requested')
      AND run.lease_owner = p_worker_id AND run.lease_token = p_lease_token
      AND run.session_fencing_token = p_fencing_token
      AND writer.run_id = p_run_id AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_fencing_token
      AND writer.expires_at_ms > p_now_ms;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;

    INSERT INTO cloud_session_questions
        (tenant_id, user_id, session_id, run_id, question_id, question, created_at_ms)
    VALUES
        (job.tenant_id, job.user_id, job.session_id, job.run_id,
         p_question_id, p_question, p_now_ms)
    ON CONFLICT (tenant_id, session_id, question_id) DO NOTHING;
    IF FOUND THEN
        RETURN TRUE;
    END IF;

    SELECT stored.question INTO existing_question
    FROM cloud_session_questions AS stored
    WHERE stored.tenant_id = job.tenant_id
      AND stored.session_id = job.session_id
      AND stored.question_id = p_question_id
      AND stored.run_id = job.run_id;
    RETURN FOUND AND existing_question = p_question;
END;
$$;

CREATE FUNCTION ternilo_cloud_question_answer(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_question_id TEXT,
    p_now_ms BIGINT
) RETURNS JSONB
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT question.answer
    FROM cloud_runs AS run
    JOIN cloud_session_writer_leases AS writer
      ON writer.tenant_id = run.tenant_id AND writer.session_id = run.session_id
    JOIN cloud_session_questions AS question
      ON question.tenant_id = run.tenant_id
     AND question.session_id = run.session_id
     AND question.run_id = run.run_id
    WHERE run.tenant_id = p_tenant_id AND run.run_id = p_run_id
      AND run.state IN ('running', 'cancel_requested')
      AND run.lease_owner = p_worker_id AND run.lease_token = p_lease_token
      AND run.session_fencing_token = p_fencing_token
      AND writer.run_id = p_run_id AND writer.lease_owner = p_worker_id
      AND writer.fencing_token = p_fencing_token
      AND writer.expires_at_ms > p_now_ms
      AND question.question_id = p_question_id
      AND question.state = 'answered'
$$;

REVOKE ALL ON FUNCTION ternilo_record_cloud_question(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, BIGINT
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_cloud_question_answer(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, BIGINT
) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_record_cloud_question(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, JSONB, BIGINT
        ) TO ternilo_worker;
        GRANT EXECUTE ON FUNCTION ternilo_cloud_question_answer(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, BIGINT
        ) TO ternilo_worker;
    END IF;
END;
$$;
