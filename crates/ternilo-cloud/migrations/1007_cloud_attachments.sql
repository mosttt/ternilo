CREATE TABLE cloud_attachment_objects (
    tenant_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    digest TEXT NOT NULL CHECK (digest ~ '^[0-9a-f]{64}$'),
    content BYTEA NOT NULL CHECK (octet_length(content) BETWEEN 1 AND 67108864),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (tenant_id, workspace_id, digest),
    FOREIGN KEY (tenant_id, workspace_id)
        REFERENCES control_workspaces(tenant_id, workspace_id) ON DELETE CASCADE
);

ALTER TABLE cloud_attachment_objects ENABLE ROW LEVEL SECURITY;

CREATE POLICY cloud_attachment_objects_tenant_scope ON cloud_attachment_objects
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE FUNCTION ternilo_store_cloud_attachment(
    p_tenant_id TEXT,
    p_run_id TEXT,
    p_worker_id TEXT,
    p_lease_token BIGINT,
    p_fencing_token BIGINT,
    p_digest TEXT,
    p_content BYTEA,
    p_now_ms BIGINT
) RETURNS BOOLEAN
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    job cloud_runs%ROWTYPE;
    existing_content BYTEA;
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

    INSERT INTO cloud_attachment_objects
        (tenant_id, workspace_id, digest, content, created_at_ms)
    VALUES
        (job.tenant_id, job.workspace_id, p_digest, p_content, p_now_ms)
    ON CONFLICT (tenant_id, workspace_id, digest) DO NOTHING;
    IF FOUND THEN
        RETURN TRUE;
    END IF;

    SELECT stored.content INTO existing_content
    FROM cloud_attachment_objects AS stored
    WHERE stored.tenant_id = job.tenant_id
      AND stored.workspace_id = job.workspace_id
      AND stored.digest = p_digest;
    RETURN FOUND AND existing_content = p_content;
END;
$$;

REVOKE ALL ON FUNCTION ternilo_store_cloud_attachment(
    TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, BYTEA, BIGINT
) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_store_cloud_attachment(
            TEXT, TEXT, TEXT, BIGINT, BIGINT, TEXT, BYTEA, BIGINT
        ) TO ternilo_worker;
    END IF;
END;
$$;
